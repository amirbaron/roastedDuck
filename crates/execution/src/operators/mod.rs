//! Physical operator implementations.

mod alter;
mod column_data_scan;
mod copy_to_file;
mod create_table;
mod create_type;
mod cross_product;
mod cte;
mod ddl;
mod delete;
mod delim;
mod dummy_scan;
mod explain_analyze;
mod export;
mod expression_filter;
mod expression_scan;
mod filter;
mod hash_aggregate;
mod insert;
pub mod join;
mod limit;
mod merge_into;
mod modify;
mod order;
mod perfect_hash_aggregate;
mod pivot;
mod projection;
mod result_collector;
mod sample;
mod seq_scan;
mod streaming_window;
mod table_inout;
mod table_scan;
mod top_n;
mod ungrouped_aggregate;
mod union;
mod unnest;
mod update;
mod window;

pub use alter::AlterEntryAction;
pub use column_data_scan::PhysicalColumnDataScan;
pub use copy_to_file::{CopyGlobal, CopyToFileOptions, PhysicalCopyToFile};
pub use create_table::PhysicalCreateTable;
pub use create_type::{CreateTypeGlobal, PhysicalCreateType};
pub use cross_product::{CrossProductExecutor, CrossProductGlobal, PhysicalCrossProduct};
pub use cte::{CteData, PhysicalCte, PhysicalCteScan, PhysicalRecursiveCte, RecursiveCteState};
pub use ddl::{
    CatalogTableCreator, CreateEntryAction, CreateIndexAction, CreateSchemaAction, CreateSequenceAction,
    CreateViewAction, DdlAction, DropAction, PhysicalDdl,
};
pub use delete::PhysicalDelete;
pub use delim::{DelimData, DelimGlobal, DelimShared, PhysicalDelimJoin, PhysicalDelimScan};
pub use dummy_scan::PhysicalDummyScan;
pub use explain_analyze::PhysicalExplainAnalyze;
pub use export::PhysicalExport;
pub use expression_filter::{ExecutableExpressionFilter, bind_expression_filters};
pub use expression_scan::{ExpressionScanState, PhysicalExpressionScan};
pub use filter::{FilterState, PhysicalFilter};
pub use hash_aggregate::{HashAggregateGlobal, HashAggregateLocal, PhysicalHashAggregate};
pub use insert::{InsertGlobal, InsertTarget, PhysicalInsert, TableCreator};
pub use join::{
    PhysicalAsOfJoin, PhysicalBlockwiseNLJoin, PhysicalHashJoin, PhysicalIEJoin, PhysicalNestedLoopJoin,
    PhysicalPiecewiseMergeJoin, PhysicalPositionalJoin,
};
pub use limit::{
    LimitBounds, LimitPercentGlobal, LimitPercentSource, PhysicalLimitPercent, PhysicalStreamingLimit,
};
pub use merge_into::{MergeGlobal, MergeIntoOperator, PhysicalMergeInto};
pub use modify::{ModifyResult, SharedAppend};
pub use order::{OrderGlobalState, OrderLocalState, OrderSourceState, PhysicalOrder};
pub use perfect_hash_aggregate::{
    PerfectHashAggregateGlobal, PerfectHashAggregateLocal, PhysicalPerfectHashAggregate,
};
pub use pivot::PhysicalPivot;
pub use projection::PhysicalProjection;
pub use result_collector::{CollectedResult, PhysicalResultCollector};
pub use sample::{BlockingSample, PhysicalReservoirSample, PhysicalStreamingSample};
pub use seq_scan::{
    DEFAULT_INDEX_SCAN_MAX_COUNT, DEFAULT_INDEX_SCAN_PERCENTAGE, PhysicalSeqScan, ScanExplainInfo, SeqScanLocal,
};
pub use streaming_window::{PhysicalStreamingWindow, StreamingWindowGlobalState, StreamingWindowState};
pub use table_inout::{InOutState, PhysicalTableInOutFunction};
pub use table_scan::{PhysicalTableScan, TableScanGlobal};
pub use top_n::{PhysicalTopN, TopNGlobalState, TopNHeap, TopNSourceState};
pub use ungrouped_aggregate::{AggregateStates, PhysicalUngroupedAggregate, UngroupedGlobal, UngroupedLocal};
pub use union::PhysicalUnion;
pub use unnest::{PhysicalUnnest, UnnestState};
pub use update::{PhysicalUpdate, UpdateGlobal};
pub use window::{
    PhysicalWindow, WindowGlobalSinkState, WindowGlobalSourceState, WindowLocalSinkState,
    WindowLocalSourceState,
};
