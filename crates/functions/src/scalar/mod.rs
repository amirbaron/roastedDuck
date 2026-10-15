//! Scalar functions, organised like DuckDB's `src/function/scalar` and
//! `extension/core_functions/scalar`.

pub(crate) mod executor;
pub mod operator;

use rustdb_function::ScalarFunctionSet;

use operator::arithmetic;

/// Every builtin scalar function set, aliases included.
pub fn scalar_functions() -> Vec<ScalarFunctionSet> {
    vec![
        arithmetic::add_functions(),
        arithmetic::subtract_functions(),
        arithmetic::multiply_functions(),
        arithmetic::divide_functions(),
        arithmetic::integer_divide_functions(),
        arithmetic::modulo_functions(),
        crate::aggregate::nested::binned_histogram::is_histogram_other_bin_function_set(),
    ]
}
