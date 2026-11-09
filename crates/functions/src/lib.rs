//! DuckDB's builtin functions, ported 1:1 (same names, overloads, results and errors).

pub mod aggregate;
mod bitstring;
mod digest;
mod dispatch;
mod limits;
mod operator;
pub mod scalar;
mod signature;
pub mod strtime;
pub mod table;
pub mod temporal;
mod values;

use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_function::{AggregateFunctionSet, ScalarFunctionSet, TableFunctionSet};

/// Every builtin scalar function set, ready to be registered in the system catalog.
pub fn builtin_scalar_functions() -> Vec<ScalarFunctionSet> {
    scalar::scalar_functions()
}

/// Every builtin aggregate function set, ready to be registered in the system catalog.
pub fn builtin_aggregate_functions() -> Result<Vec<AggregateFunctionSet>> {
    aggregate::aggregate_functions()
}

/// Every builtin table function set, ready to be registered in the system catalog.
pub fn builtin_table_functions() -> Vec<TableFunctionSet> {
    let mut sets = table::range::range_functions();
    sets.push(table::test_all_types::test_all_types_functions());
    sets.push(table::repeat::repeat_functions());
    sets.push(table::repeat::repeat_row_functions());
    sets.push(table::unnest::unnest_functions());
    sets.push(table::summary::summary_functions());
    sets.push(table::test_vector_types::test_vector_types_functions());
    sets.push(table::query_function::query_functions());
    sets.push(table::query_function::query_table_functions());
    sets
}

/// DuckDB's `ParameterNotResolvedException`, raised by bind callbacks that need the type of
/// a prepared statement parameter.
pub fn parameter_not_resolved() -> Error {
    Error::new(
        ErrorKind::ParameterNotResolved,
        "Parameter types could not be resolved",
    )
}
