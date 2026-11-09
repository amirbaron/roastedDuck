//! `strip_accents` (DuckDB's `strip_accents.cpp`) and `nfc_normalize`
//! (`nfc_normalize.cpp`).

use roastedduck_function::{ScalarFunction, ScalarFunctionSet};
use roastedduck_types::LogicalType;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::scalar::executor::{Str, StrOut, unary};
use crate::scalar::string::utf8::text;

/// `utf8proc_remove_accents`: decompose, drop combining marks (`UTF8PROC_STRIPMARK`)
/// and recompose.
fn strip_accents(input: &[u8], out: &mut Vec<u8>) {
    if input.is_ascii() {
        out.extend_from_slice(input);
        return;
    }
    let stripped: String = text(input)
        .nfd()
        .filter(|&c| !is_combining_mark(c))
        .nfc()
        .collect();
    out.extend_from_slice(stripped.as_bytes());
}

/// NFC normalization (DuckDB's `Utf8Proc::Normalize`).
fn nfc_normalize(input: &[u8], out: &mut Vec<u8>) {
    if input.is_ascii() {
        out.extend_from_slice(input);
        return;
    }
    let normalized: String = text(input).nfc().collect();
    out.extend_from_slice(normalized.as_bytes());
}

fn string_set(name: &str, op: fn(&[u8], &mut Vec<u8>)) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::VARCHAR],
        LogicalType::VARCHAR,
        move |args, _, result| {
            unary::<Str, StrOut>(args, result, |input, out| {
                op(input, out);
                Ok(true)
            })
        },
    ));
    set
}

/// `strip_accents(string)`.
pub fn strip_accents_functions() -> ScalarFunctionSet {
    string_set("strip_accents", strip_accents)
}

/// `nfc_normalize(string)`.
pub fn nfc_normalize_functions() -> ScalarFunctionSet {
    string_set("nfc_normalize", nfc_normalize)
}
