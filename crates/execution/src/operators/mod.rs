//! Physical operator implementations.

mod column_data_scan;
mod create_table;
mod delete;
mod dummy_scan;
mod filter;
mod insert;
mod limit;
mod modify;
mod projection;
mod result_collector;
mod seq_scan;
mod ungrouped_aggregate;
mod update;

pub use column_data_scan::PhysicalColumnDataScan;
pub use create_table::PhysicalCreateTable;
pub use delete::PhysicalDelete;
pub use dummy_scan::PhysicalDummyScan;
pub use filter::{FilterState, PhysicalFilter};
pub use insert::{InsertGlobal, InsertTarget, PhysicalInsert, TableCreator};
pub use limit::PhysicalStreamingLimit;
pub use modify::ModifyResult;
pub use projection::PhysicalProjection;
pub use result_collector::{CollectedResult, PhysicalResultCollector};
pub use seq_scan::{PhysicalSeqScan, SeqScanLocal};
pub use ungrouped_aggregate::{AggregateStates, PhysicalUngroupedAggregate, UngroupedLocal};
pub use update::{PhysicalUpdate, UpdateGlobal};
