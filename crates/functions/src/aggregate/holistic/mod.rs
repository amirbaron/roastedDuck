//! Holistic aggregates (DuckDB's `core_functions/aggregate/holistic`): aggregates that need
//! all of their input, such as quantiles and the mode.

pub mod mode;
pub mod quantile;
