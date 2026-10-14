//! Physical operator implementations.

mod column_data_scan;
mod dummy_scan;
mod filter;
mod limit;
mod projection;
mod result_collector;
mod ungrouped_aggregate;

pub use column_data_scan::PhysicalColumnDataScan;
pub use dummy_scan::PhysicalDummyScan;
pub use filter::{FilterState, PhysicalFilter};
pub use limit::PhysicalStreamingLimit;
pub use projection::PhysicalProjection;
pub use result_collector::{CollectedResult, PhysicalResultCollector};
pub use ungrouped_aggregate::{AggregateStates, PhysicalUngroupedAggregate, UngroupedLocal};
