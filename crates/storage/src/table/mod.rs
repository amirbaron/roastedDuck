//! Table storage internals (DuckDB's `storage/table`): row groups, column data, column
//! segments, version information and updates.

pub mod adaptive_filter;
pub mod chunk_info;
pub mod column_data;
pub mod column_segment;
pub(crate) mod geo_column_data;
pub mod per_column_metadata_blocks;
pub mod row_group;
pub mod row_group_collection;
pub mod row_group_reorderer;
pub mod row_version_manager;
pub mod scan_state;
pub mod segment_tree;
pub mod update_segment;
pub mod variant_column_data;

pub use adaptive_filter::AdaptiveFilter;
pub use chunk_info::{
    ChunkVectorInfo, DeletedRows, DeletedScanType, InsertedScanType, ScanOptions, UpdateScanType,
};
pub use column_data::{
    ColumnAppendState, ColumnData, ColumnDataType, ColumnKind, ColumnScanState, ColumnSegmentInfo,
    UpdateRegistration,
};
pub use column_segment::{ColumnSegment, SegmentScanState, SegmentType};
pub use per_column_metadata_blocks::{PerColumnMetadataBlock, PerColumnMetadataBlocks};
pub use row_group::{
    DeleteRecord, RowGroup, RowGroupAppendState, RowGroupMetadata, TableScanType, UpdateRecord,
};
pub use row_group_collection::{
    FetchType, RowGroupAppendMode, RowGroupCollection, RowGroupSegmentTree, TableAppendState,
    TableMetadataPointer, TableStatistics,
};
pub use row_group_reorderer::{
    OffsetPruningResult, OrderByColumnType, OrderByStatistics, RowGroupOrderOptions, RowGroupPartition,
};
pub use row_version_manager::RowVersionManager;
pub use scan_state::{
    CollectionScanState, ParallelCollectionScanState, ParallelTableScanState, ScanFilter, ScanFilterInfo,
    ScanRandom, ScanSampling, StorageIndex, TableScanOptions, TableScanState,
};
pub use segment_tree::{Segment, SegmentNode, SegmentTree};
pub use update_segment::{UpdateSegment, UpdateVersion};
pub use variant_column_data::VariantShreddingConfig;
