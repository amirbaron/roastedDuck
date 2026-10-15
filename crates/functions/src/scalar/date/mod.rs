//! Date and time functions (DuckDB's `function/scalar/date` and `core_functions/scalar/date`).

pub(crate) mod calendar;
pub mod date_diff;
pub mod date_part;
pub mod date_trunc;
pub mod to_interval;
