//! `ascii` (DuckDB's `ascii.cpp`), `chr` (`chr.cpp`) and `unicode`/`ord`
//! (`unicode.cpp`).

use roastedduck_common::Error;
use roastedduck_function::{ScalarFunction, ScalarFunctionSet};
use roastedduck_types::LogicalType;

use crate::scalar::executor::{Str, StrOut, unary};
use crate::scalar::string::utf8::{decode_first, push_char};

fn first_codepoint_set(name: &str, empty: i32) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::VARCHAR],
        LogicalType::INTEGER,
        move |args, _, result| {
            unary::<Str, i32>(args, result, |input, out| {
                *out = match decode_first(input) {
                    Some((c, _)) => u32::from(c) as i32,
                    None => input.first().map_or(empty, |&byte| i32::from(byte)),
                };
                Ok(true)
            })
        },
    ));
    set
}

/// `ascii(string)`: the code point of the first character, 0 for the empty string.
pub fn ascii_functions() -> ScalarFunctionSet {
    first_codepoint_set("ascii", 0)
}

/// `unicode(string)`: the code point of the first character, -1 for the empty string
/// (`utf8proc_iterate` on empty input).
pub fn unicode_functions() -> ScalarFunctionSet {
    first_codepoint_set("unicode", -1)
}

/// `chr(code_point)`.
pub fn chr_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("chr");
    set.add(ScalarFunction::new(
        "chr",
        vec![LogicalType::INTEGER],
        LogicalType::VARCHAR,
        |args, _, result| {
            unary::<i32, StrOut>(args, result, |code, out| {
                let c = u32::try_from(code)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| Error::invalid_input(format!("Invalid UTF8 Codepoint {code}")))?;
                push_char(out, c);
                Ok(true)
            })
        },
    ));
    set
}
