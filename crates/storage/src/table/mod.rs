//! Table storage internals (DuckDB's `storage/table`): row groups, column data, column
//! segments, version information and updates.

pub mod chunk_info;
pub mod column_data;
pub mod column_segment;
pub mod row_group;
pub mod row_group_collection;
pub mod row_version_manager;
pub mod scan_state;
pub mod segment_tree;
pub mod update_segment;

pub use chunk_info::{
    ChunkVectorInfo, DeletedRows, DeletedScanType, InsertedScanType, ScanOptions, UpdateScanType,
};
pub use column_data::{
    ColumnAppendState, ColumnData, ColumnDataType, ColumnKind, ColumnScanState, ColumnSegmentInfo,
    UpdateRegistration,
};
pub use column_segment::{ColumnSegment, SegmentScanState, SegmentType};
pub use row_group::{DeleteRecord, RowGroup, RowGroupAppendState, TableScanType, UpdateRecord};
pub use row_group_collection::{
    FetchType, RowGroupAppendMode, RowGroupCollection, RowGroupSegmentTree, TableAppendState, TableStatistics,
};
pub use row_version_manager::RowVersionManager;
pub use scan_state::{
    CollectionScanState, ParallelCollectionScanState, ParallelTableScanState, ScanFilter, ScanFilterInfo,
    StorageIndex, TableScanOptions, TableScanState,
};
pub use segment_tree::{Segment, SegmentNode, SegmentTree};
pub use update_segment::{UpdateSegment, UpdateVersion};
