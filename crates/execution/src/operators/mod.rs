//! Physical operator implementations.

mod column_data_scan;
mod dummy_scan;
mod filter;
mod projection;
mod result_collector;

pub use column_data_scan::PhysicalColumnDataScan;
pub use dummy_scan::PhysicalDummyScan;
pub use filter::{FilterState, PhysicalFilter};
pub use projection::PhysicalProjection;
pub use result_collector::{CollectedResult, PhysicalResultCollector};
