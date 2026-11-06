//! Row-format data for the operators that materialise: hash join, hash aggregate, sort
//! and window (DuckDB's `common/types/row`, `common/row_operations`, `common/sort` and
//! radix partitioning).
//!
//! # Using a [`TupleDataCollection`]
//! ```text
//! let layout = Arc::new(TupleDataLayout::new(types, ValidityType::CanHaveNull)?);
//! let mut rows = TupleDataCollection::new(buffer_manager, layout, MemoryTag::HashTable)?;
//! let mut append = rows.initialize_append(PinProperties::KeepEverythingPinned);
//! rows.append(&mut append, &chunk)?;                // row pointers: append.chunk_state.row_locations()
//! rows.finalize_pin_state(&mut append.pin_state);
//! let mut scan = rows.initialize_scan(PinProperties::UnpinAfterDone);
//! while rows.scan(&mut scan, &mut out)? { ... }
//! ```
//! Partitioned sinks use [`PartitionedTupleData`]; sorts use [`sort::Sort`].
//!
//! # Memory
//! Rows and their variable-size data live in blocks of the buffer manager, so they count
//! against the memory limit and can be evicted (and spilled) when not pinned. Rows hold
//! absolute heap pointers, which are recomputed when an evicted heap block is reloaded
//! elsewhere. Gathered strings point into the heap rather than being copied; the vectors
//! keep the heap blocks pinned.

mod aggregate;
mod collection;
mod column_data;
mod compare;
mod format;
mod gather;
pub mod hash;
mod heap;
mod layout;
mod matcher;
mod partitioned;
mod radix;
mod row;
mod scatter;
mod segment;
pub mod sort;
mod state;

pub use aggregate::{
    GroupedUpdate, RowOperationsState, combine_states, destroy_states, finalize_states, initialize_states,
    supports_update_selection, update_state_selection, update_states,
};
pub use collection::{ChunkIterator, TupleDataCollection};
pub use column_data::{ColumnDataAppendState, ColumnDataCollection, ColumnDataScanState};
pub use compare::{compare_values, values_distinct};
pub use format::{ChunkFormat, VectorFormat};
pub use gather::HeapKeepAlive;
pub use layout::{RowAggregate, SortKeyColumn, TupleDataLayout, ValidityType};
pub use matcher::{MatchPredicate, RowMatcher};
pub use partitioned::{PartitionedAppendState, PartitionedTupleData};
pub use radix::RadixPartitioning;
pub use row::RowPtr;
pub use state::{AppendState, ChunkState, ParallelScanState, PinProperties, PinState, ScanState};
