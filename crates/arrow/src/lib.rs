//! The Arrow C data interface (DuckDB's `src/common/arrow` and
//! `src/function/table/arrow*`).
//!
//! Exporting: [`to_arrow_schema`] describes columns as an Arrow schema and
//! [`to_arrow_array`] (an [`ArrowAppender`]) converts data chunks into Arrow arrays whose
//! buffers they own until the consumer releases them. Importing: an Arrow schema's
//! fields become [`ArrowType`]s ([`populate_arrow_table_schema`]), whose arrays
//! [`data_chunk_from_arrow`] turns into data chunks; the `arrow_scan` table function
//! ([`arrow_table_functions`]) scans an Arrow stream the same way.
//!
//! The structs of the C interface are in [`ffi`]; the settings that shape the output
//! (`arrow_large_buffer_size`, `arrow_output_version`, ...) are [`ClientProperties`].
//! Types without an exact Arrow counterpart travel as Arrow extension types.
#![allow(unsafe_code)]

mod appender;
mod client_properties;
mod conversion;
mod converter;
mod duck_schema;
pub mod ffi;
mod schema_metadata;
mod table_function;
mod type_extension;

pub use appender::ArrowAppender;
pub use client_properties::{ArrowFormatVersion, ArrowOffsetSize, ClientProperties};
pub use conversion::data_chunk_from_arrow;
pub use converter::{to_arrow_array, to_arrow_schema};
pub use duck_schema::{ArrowArrayPhysicalType, ArrowTableSchema, ArrowType};
pub use roastedduck_storage::{TableFilter, TableFilterSet};
pub use table_function::{
    ArrowProjectedColumns, ArrowStreamParameters, StreamFactoryGetSchema, StreamFactoryProduce,
    arrow_table_functions, populate_arrow_table_schema, retain_factory_owner,
};

#[cfg(test)]
mod tests;
