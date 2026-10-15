//! Physical operator implementations.

mod column_data_scan;
mod dummy_scan;
mod filter;
mod hash_aggregate;
mod limit;
mod nested_loop_join;
mod order;
mod projection;
mod result_collector;
mod table_scan;
mod ungrouped_aggregate;
mod union;

pub use column_data_scan::PhysicalColumnDataScan;
pub use dummy_scan::PhysicalDummyScan;
pub use filter::{FilterState, PhysicalFilter};
pub use hash_aggregate::{HashAggregateGlobal, HashAggregateLocal, PhysicalHashAggregate};
pub use limit::PhysicalStreamingLimit;
pub use nested_loop_join::{
    NestedLoopJoinGlobal, NestedLoopJoinProbe, NestedLoopJoinSinkLocal, PhysicalNestedLoopJoin,
};
pub use order::{OrderGlobalState, OrderLocalState, OrderSourceState, PhysicalOrder};
pub use projection::PhysicalProjection;
pub use result_collector::{CollectedResult, PhysicalResultCollector};
pub use table_scan::{PhysicalTableScan, TableScanGlobal};
pub use ungrouped_aggregate::{AggregateStates, PhysicalUngroupedAggregate, UngroupedLocal};
pub use union::PhysicalUnion;
