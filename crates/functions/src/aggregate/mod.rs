//! Aggregate functions, organised like DuckDB's `src/function/aggregate` and
//! `extension/core_functions/aggregate`.

pub mod algebraic;
pub(crate) mod collect;
pub mod distributive;
pub(crate) mod heap;
pub mod holistic;
pub mod nested;
pub mod regression;
pub mod sorted;
pub(crate) mod stored;

use rustdb_common::Result;
use rustdb_function::AggregateFunctionSet;

use algebraic::{avg, stddev};
use distributive::{
    approx_count, arg_min_max, bitagg, bitstring_agg, bool, count, first_last_any, minmax, moments, product,
    string_agg, sum,
};
use holistic::{approx_quantile, approx_top_k, mode, quantile, reservoir_quantile};
use nested::{binned_histogram, list};

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
        bitstring_agg::bitstring_agg_functions(),
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
    let arg_min = arg_min_max::arg_min_functions();
    let arg_max = arg_min_max::arg_max_functions();
    let string_agg = string_agg::string_agg_functions();
    let list = list::list_functions();
    let quantile_disc = quantile::quantile_disc_functions();
    sets.extend([
        arg_min.clone().renamed("argmin"),
        arg_min.clone().renamed("min_by"),
        arg_min,
        arg_max.clone().renamed("argmax"),
        arg_max.clone().renamed("max_by"),
        arg_max,
        arg_min_max::arg_min_null_functions(),
        arg_min_max::arg_max_null_functions(),
        arg_min_max::arg_min_nulls_last_functions(),
        arg_min_max::arg_max_nulls_last_functions(),
        string_agg.clone().renamed("listagg"),
        string_agg.clone().renamed("group_concat"),
        string_agg,
        list.clone().renamed("array_agg"),
        list,
        binned_histogram::histogram_functions(),
        binned_histogram::histogram_exact_functions(),
        mode::mode_functions(),
        mode::entropy_functions(),
        approx_top_k::approx_top_k_functions(),
        approx_quantile::approx_quantile_functions()?,
        reservoir_quantile::reservoir_quantile_functions()?,
        quantile_disc.clone().renamed("quantile"),
        quantile_disc,
        quantile::quantile_cont_functions(),
        quantile::median_functions(),
        quantile::mad_functions()?,
    ]);
    sets.extend(regression::regression_functions());
    Ok(sets)
}
