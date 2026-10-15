//! Physical operator implementations.

mod column_data_scan;
mod create_table;
mod delete;
mod delim;
mod dummy_scan;
mod filter;
mod hash_aggregate;
mod insert;
mod limit;
mod modify;
mod nested_loop_join;
mod order;
mod projection;
mod result_collector;
mod seq_scan;
mod table_scan;
mod ungrouped_aggregate;
mod union;
mod update;

pub use column_data_scan::PhysicalColumnDataScan;
pub use create_table::PhysicalCreateTable;
pub use delete::PhysicalDelete;
pub use delim::{DelimData, DelimGlobal, DelimShared, PhysicalDelimJoin, PhysicalDelimScan};
pub use dummy_scan::PhysicalDummyScan;
pub use filter::{FilterState, PhysicalFilter};
pub use hash_aggregate::{HashAggregateGlobal, HashAggregateLocal, PhysicalHashAggregate};
pub use insert::{InsertGlobal, InsertTarget, PhysicalInsert, TableCreator};
pub use limit::PhysicalStreamingLimit;
pub use modify::ModifyResult;
pub use nested_loop_join::{
    NestedLoopJoinGlobal, NestedLoopJoinProbe, NestedLoopJoinSinkLocal, PhysicalNestedLoopJoin,
};
pub use order::{OrderGlobalState, OrderLocalState, OrderSourceState, PhysicalOrder};
pub use projection::PhysicalProjection;
pub use result_collector::{CollectedResult, PhysicalResultCollector};
pub use seq_scan::{PhysicalSeqScan, SeqScanLocal};
pub use table_scan::{PhysicalTableScan, TableScanGlobal};
pub use ungrouped_aggregate::{AggregateStates, PhysicalUngroupedAggregate, UngroupedLocal};
pub use union::PhysicalUnion;
pub use update::{PhysicalUpdate, UpdateGlobal};
