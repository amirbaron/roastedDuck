//! Date and time functions (DuckDB's `function/scalar/date` and `core_functions/scalar/date`).

pub(crate) mod calendar;
pub mod date_diff;
pub mod date_part;
pub mod date_trunc;
pub mod make_date;
pub mod strftime;
pub(crate) mod strftime_format;
pub mod time_bucket;
pub mod to_interval;
