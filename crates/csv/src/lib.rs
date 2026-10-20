//! The CSV reader and writer (DuckDB's `execution/operator/csv_scanner` and
//! `function/table/copy_csv.cpp`).

pub mod buffer;
pub mod copy_blob;
pub mod copy_csv;
pub mod error;
pub mod file_scan;
pub mod multi_file;
pub mod options;
pub mod read_csv;
pub mod scanner;
pub mod schema;
pub mod sniff_csv;
pub mod sniffer;
pub mod state_machine;
