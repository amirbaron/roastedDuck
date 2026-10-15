//! DuckDB's builtin functions, ported 1:1 (same names, overloads, results and errors).

pub mod aggregate;
mod bitstring;
mod dispatch;
mod operator;
pub mod scalar;
pub mod table;
mod temporal;

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::{AggregateFunctionSet, ScalarFunctionSet, TableFunctionSet};

/// Every builtin scalar function set, ready to be registered in the system catalog.
pub fn builtin_scalar_functions() -> Vec<ScalarFunctionSet> {
    scalar::operators::arithmetic_functions()
}

/// Every builtin aggregate function set, ready to be registered in the system catalog.
pub fn builtin_aggregate_functions() -> Result<Vec<AggregateFunctionSet>> {
    aggregate::aggregate_functions()
}

/// Every builtin table function set, ready to be registered in the system catalog.
pub fn builtin_table_functions() -> Vec<TableFunctionSet> {
    table::range::range_functions()
}

/// DuckDB's `ParameterNotResolvedException`, raised by bind callbacks that need the type of
/// a prepared statement parameter.
pub(crate) fn parameter_not_resolved() -> Error {
    Error::new(
        ErrorKind::ParameterNotResolved,
        "Parameter types could not be resolved",
    )
}
