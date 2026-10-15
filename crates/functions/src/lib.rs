//! DuckDB's builtin functions, ported 1:1 (same names, overloads, results and errors).

pub mod scalar;
pub mod table;

use rustdb_function::{ScalarFunctionSet, TableFunctionSet};

/// Every builtin scalar function set, ready to be registered in the system catalog.
pub fn builtin_scalar_functions() -> Vec<ScalarFunctionSet> {
    scalar::operators::arithmetic_functions()
}

/// Every builtin table function set, ready to be registered in the system catalog.
pub fn builtin_table_functions() -> Vec<TableFunctionSet> {
    table::range::range_functions()
}
