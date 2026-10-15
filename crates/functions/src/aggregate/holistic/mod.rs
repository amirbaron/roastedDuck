//! Holistic aggregates (DuckDB's `core_functions/aggregate/holistic`): aggregates that need
//! all of their input, such as quantiles and the mode.

pub mod approx_top_k;
pub mod mode;
pub mod quantile;
