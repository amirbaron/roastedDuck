//! `md5`, `md5_number` (DuckDB's `md5.cpp`), `sha1` (`sha1.cpp`) and `sha256`
//! (`sha256.cpp`).

use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_types::LogicalType;

use crate::digest::{md5, push_hex, sha1, sha256};
use crate::scalar::executor::{Str, StrOut, unary};

/// A hex digest function over VARCHAR and BLOB.
fn hex_digest_set(name: &str, digest: fn(&[u8], &mut Vec<u8>)) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    for ty in [LogicalType::VARCHAR, LogicalType::BLOB] {
        set.add(ScalarFunction::new(
            name,
            vec![ty],
            LogicalType::VARCHAR,
            move |args, _, result| {
                unary::<Str, StrOut>(args, result, |input, out| {
                    digest(input, out);
                    Ok(true)
                })
            },
        ));
    }
    set
}

/// `md5(string)`: the hex MD5 digest.
pub fn md5_functions() -> ScalarFunctionSet {
    hex_digest_set("md5", |input, out| push_hex(out, &md5(input)))
}

/// `md5_number(string)`: the MD5 digest read as a little-endian UHUGEINT.
pub fn md5_number_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("md5_number");
    for ty in [LogicalType::VARCHAR, LogicalType::BLOB] {
        set.add(ScalarFunction::new(
            "md5_number",
            vec![ty],
            LogicalType::UHUGEINT,
            |args, _, result| {
                unary::<Str, u128>(args, result, |input, out| {
                    *out = u128::from_le_bytes(md5(input));
                    Ok(true)
                })
            },
        ));
    }
    set
}

/// `sha1(string)`: the hex SHA-1 digest.
pub fn sha1_functions() -> ScalarFunctionSet {
    hex_digest_set("sha1", |input, out| push_hex(out, &sha1(input)))
}

/// `sha256(string)`: the hex SHA-256 digest.
pub fn sha256_functions() -> ScalarFunctionSet {
    hex_digest_set("sha256", |input, out| push_hex(out, &sha256(input)))
}
