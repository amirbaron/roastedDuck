//! The Parquet extension (DuckDB's `extension/parquet`): the `read_parquet` table
//! function and its replacement scan, the `parquet_*` metadata functions and the
//! `COPY ... (FORMAT parquet)` writer.
//!
//! The module layout follows DuckDB's: [`thrift`] holds the metadata structures,
//! [`file`] reads footers, [`schema`] maps Parquet schemas to DuckDB types,
//! [`column_reader`] decodes column chunks (with the decoders of [`decode`] and the codecs
//! of [`compression`]), [`reader`] scans one file and [`scan`] is the multi-file table
//! function.

pub mod column_reader;
pub mod compression;
pub mod copy;
pub mod crypto;
pub mod decode;
pub mod file;
pub mod filter;
pub mod geo;
pub mod metadata;
pub mod reader;
pub mod scan;
pub mod schema;
pub mod snappy;
pub mod statistics;
pub mod thrift;
pub mod variant;
pub mod vector_ops;
pub mod writer;

use rustdb_function::{ScalarFunctionSet, TableFunctionSet};

/// The table functions of the extension.
pub fn parquet_table_functions() -> Vec<TableFunctionSet> {
    let mut functions = scan::read_parquet_functions();
    functions.extend(metadata::metadata_functions());
    functions
}

/// The scalar functions of the extension: `variant_to_parquet_variant` and
/// `variant_bytes_to_variant`.
pub fn parquet_scalar_functions() -> Vec<ScalarFunctionSet> {
    variant::functions()
}
