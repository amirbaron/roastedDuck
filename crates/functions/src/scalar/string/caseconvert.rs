//! `lower`/`lcase` and `upper`/`ucase` (DuckDB's `caseconvert.cpp`), using utf8proc's
//! simple (one-to-one) case mappings.

use roastedduck_function::{ScalarFunction, ScalarFunctionSet};
use roastedduck_types::LogicalType;

use crate::scalar::executor::{Str, StrOut, unary};
use crate::scalar::string::utf8::{push_char, simple_lower, simple_upper, text};

/// Appends `input` with every character case-converted (DuckDB's `CaseConvert`).
pub(crate) fn case_convert(input: &[u8], upper: bool, out: &mut Vec<u8>) {
    if input.is_ascii() {
        out.extend(input.iter().map(|byte| {
            if upper {
                byte.to_ascii_uppercase()
            } else {
                byte.to_ascii_lowercase()
            }
        }));
        return;
    }
    for c in text(input).chars() {
        if c.is_ascii() {
            out.push(if upper {
                c.to_ascii_uppercase() as u8
            } else {
                c.to_ascii_lowercase() as u8
            });
        } else {
            push_char(out, if upper { simple_upper(c) } else { simple_lower(c) });
        }
    }
}

fn case_set(name: &str, upper: bool) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::VARCHAR],
        LogicalType::VARCHAR,
        move |args, _, result| {
            unary::<Str, StrOut>(args, result, |input, out| {
                case_convert(input, upper, out);
                Ok(true)
            })
        },
    ));
    set
}

/// `lower` (DuckDB's `LowerFun`).
pub fn lower_functions() -> ScalarFunctionSet {
    case_set("lower", false)
}

/// `upper` (DuckDB's `UpperFun`).
pub fn upper_functions() -> ScalarFunctionSet {
    case_set("upper", true)
}
