//! Checkpoints: writing tables to the database file and reading them back (DuckDB's
//! `TableDataWriter`, `RowGroupWriter`, `ColumnCheckpointState` and `TableDataReader`).

pub mod pointers;
pub mod stream;
pub mod table_reader;
pub mod table_writer;
pub mod vector_serde;
