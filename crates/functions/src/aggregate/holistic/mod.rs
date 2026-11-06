//! Holistic aggregates (DuckDB's `core_functions/aggregate/holistic`): aggregates that need
//! all of their input, such as quantiles and the mode.

pub mod approx_quantile;
pub mod approx_top_k;
pub mod mode;
pub mod quantile;
pub mod reservoir_quantile;
pub(crate) mod tdigest;

/// The integer type stored in a physical type (the functions DuckDB binds for DECIMAL
/// inputs of the approximate quantiles).
pub(crate) fn integer_type_of(
    physical: rustdb_types::PhysicalType,
) -> rustdb_common::Result<rustdb_types::LogicalType> {
    use rustdb_types::{LogicalType, PhysicalType};
    Ok(match physical {
        PhysicalType::Int8 => LogicalType::TINYINT,
        PhysicalType::Int16 => LogicalType::SMALLINT,
        PhysicalType::Int32 => LogicalType::INTEGER,
        PhysicalType::Int64 => LogicalType::BIGINT,
        PhysicalType::Int128 => LogicalType::HUGEINT,
        _ => {
            return Err(rustdb_common::Error::internal(
                "Unimplemented quantile decimal aggregate",
            ));
        }
    })
}
