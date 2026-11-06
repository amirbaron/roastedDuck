//! `reverse` (DuckDB's `reverse.cpp`): reverses grapheme clusters.

use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_types::LogicalType;

use crate::scalar::executor::{Str, StrOut, unary};
use crate::scalar::string::utf8::grapheme_ranges;

fn reverse(input: &[u8], out: &mut Vec<u8>) {
    if input.is_ascii() {
        out.extend(input.iter().rev());
        return;
    }
    for (start, end) in grapheme_ranges(input).into_iter().rev() {
        out.extend_from_slice(&input[start..end]);
    }
}

/// `reverse(string)`.
pub fn reverse_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("reverse");
    set.add(ScalarFunction::new(
        "reverse",
        vec![LogicalType::VARCHAR],
        LogicalType::VARCHAR,
        |args, _, result| {
            unary::<Str, StrOut>(args, result, |input, out| {
                reverse(input, out);
                Ok(true)
            })
        },
    ));
    set
}
