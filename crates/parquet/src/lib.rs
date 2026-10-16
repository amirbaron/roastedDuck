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
pub mod decode;
pub mod file;
pub mod reader;
pub mod scan;
pub mod schema;
pub mod thrift;
pub mod vector_ops;

use rustdb_function::TableFunctionSet;

/// The table functions of the extension.
pub fn parquet_table_functions() -> Vec<TableFunctionSet> {
    scan::read_parquet_functions()
}
