//! `contains` (DuckDB's `contains.cpp`), `prefix` (`prefix.cpp`), `suffix`/`ends_with`
//! (`suffix.cpp`) and `starts_with`/`^@` (`starts_with.cpp`).

use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_types::LogicalType;

use crate::scalar::executor::{Str, binary};

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
    set.add(string_predicate("contains", |haystack, needle| {
        find(haystack, needle).is_some()
    }));
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
    predicate_set("^@", <[u8]>::starts_with)
}
