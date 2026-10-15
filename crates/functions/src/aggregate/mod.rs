//! Aggregate functions, organised like DuckDB's `src/function/aggregate` and
//! `extension/core_functions/aggregate`.

pub mod algebraic;
pub mod distributive;
pub(crate) mod heap;
pub mod regression;
pub(crate) mod stored;

use rustdb_common::Result;
use rustdb_function::AggregateFunctionSet;

use algebraic::{avg, stddev};
use distributive::{approx_count, bitagg, bool, count, first_last_any, minmax, moments, product, sum};

/// Every builtin aggregate function set, aliases included.
pub fn aggregate_functions() -> Result<Vec<AggregateFunctionSet>> {
    let first = first_last_any::first_functions();
    let kahan_sum = sum::kahan_sum_functions();
    let count_if = sum::count_if_functions()?;
    let avg = avg::avg_functions()?;
    let var_samp = stddev::var_samp_functions();
    let stddev_samp = stddev::stddev_samp_functions();
    let mut sets = vec![
        count::count_star_functions(),
        count::count_functions(),
        first.clone().renamed("arbitrary"),
        first,
        first_last_any::last_functions(),
        first_last_any::any_value_functions(),
        minmax::min_functions(),
        minmax::max_functions(),
        sum::sum_functions()?,
        sum::sum_no_overflow_functions(),
        kahan_sum.clone().renamed("fsum"),
        kahan_sum.clone().renamed("sumkahan"),
        kahan_sum,
        count_if.clone().renamed("countif"),
        count_if,
        avg.clone().renamed("mean"),
        avg,
        avg::favg_functions(),
        bool::bool_and_functions(),
        bool::bool_or_functions(),
        bitagg::bit_and_functions(),
        bitagg::bit_or_functions(),
        bitagg::bit_xor_functions(),
        product::product_functions(),
        moments::skewness_functions(),
        moments::kurtosis_functions(),
        moments::kurtosis_pop_functions(),
        approx_count::approx_count_distinct_functions(),
        var_samp.clone().renamed("variance"),
        var_samp,
        stddev::var_pop_functions(),
        stddev_samp.clone().renamed("stddev"),
        stddev_samp,
        stddev::stddev_pop_functions(),
        stddev::sem_functions(),
        stddev::covar_pop_functions(),
        stddev::covar_samp_functions(),
        stddev::corr_functions(),
    ];
    sets.extend(regression::regression_functions());
    Ok(sets)
}
