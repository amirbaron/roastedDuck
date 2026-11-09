//! `contains` (DuckDB's `contains.cpp`), `prefix` (`prefix.cpp`), `suffix`/`ends_with`
//! (`suffix.cpp`) and `starts_with`/`^@` (`starts_with.cpp`).

use roastedduck_function::{CollationHandling, FunctionState, ScalarFunction, ScalarFunctionSet};
use roastedduck_types::{DataChunk, LogicalType, Vector, VectorKind};

use crate::scalar::executor::{Str, binary, str_at, unary};

/// The byte offset of the first occurrence of `needle` in `haystack` (DuckDB's
/// `FindStrInStr`); an empty needle is found at 0.
pub(crate) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    memchr::memmem::find(haystack, needle)
}

fn predicate_set(name: &str, op: fn(&[u8], &[u8]) -> bool) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(string_predicate(name, op));
    set
}

fn string_predicate(name: &str, op: fn(&[u8], &[u8]) -> bool) -> ScalarFunction {
    ScalarFunction::new(
        name,
        vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
        LogicalType::BOOLEAN,
        move |args, _, result| {
            binary::<Str, Str, bool>(args, result, |left, right, out| {
                *out = op(left, right);
                Ok(true)
            })
        },
    )
}

/// `contains(string, search)`, plus the LIST, MAP and STRUCT overloads given in
/// `nested` (DuckDB's `ContainsFun`).
pub fn contains_functions(nested: impl IntoIterator<Item = ScalarFunction>) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("contains");
    let mut string = ScalarFunction::new(
        "contains",
        vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
        LogicalType::BOOLEAN,
        |args: &DataChunk, _: &mut FunctionState, result: &mut Vector| {
            let needle = args.column(1);
            if needle.is_constant() && args.column(0).kind() != VectorKind::Constant {
                // One searcher for every row instead of one per row.
                let Some(needle) = str_at(needle, 0) else {
                    *result = Vector::constant_null(LogicalType::BOOLEAN);
                    return Ok(());
                };
                let finder = memchr::memmem::Finder::new(needle);
                let haystacks = DataChunk::new(vec![args.column(0).clone()], args.len());
                return unary::<Str, bool>(&haystacks, result, |haystack, out| {
                    *out = finder.find(haystack).is_some();
                    Ok(true)
                });
            }
            binary::<Str, Str, bool>(args, result, |haystack, needle, out| {
                *out = find(haystack, needle).is_some();
                Ok(true)
            })
        },
    );
    string.collation_handling = CollationHandling::PushCombinableCollations;
    set.add(string);
    for mut function in nested {
        function.name = set.name.clone();
        set.add(function);
    }
    set
}

/// `prefix(string, prefix)` (DuckDB's `PrefixFun`).
pub fn prefix_functions() -> ScalarFunctionSet {
    predicate_set("prefix", <[u8]>::starts_with)
}

/// `suffix(string, suffix)` (DuckDB's `SuffixFun`).
pub fn suffix_functions() -> ScalarFunctionSet {
    predicate_set("suffix", <[u8]>::ends_with)
}

/// `^@`/`starts_with` (DuckDB's `StartsWithOperatorFun`).
pub fn starts_with_functions() -> ScalarFunctionSet {
    let mut set = predicate_set("^@", <[u8]>::starts_with);
    for function in &mut set.functions {
        function.collation_handling = CollationHandling::PushCombinableCollations;
    }
    set
}
