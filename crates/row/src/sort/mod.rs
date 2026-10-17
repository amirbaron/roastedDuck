//! Sorting (DuckDB's `common/sort`): normalized keys, sorted runs, the parallel merger and
//! the sort strategies used by ORDER BY and window functions.
//!
//! # Using a [`Sort`]
//! ```text
//! let sort = Sort::new(buffer_manager, orders, &input_types, None)?;
//! let global = sort.global_sink_state(config);          // shared by sinking threads
//! let mut local = sort.local_sink_state();              // per thread
//! sort.sink(&chunk, &global, &mut local)?;              // for every input chunk
//! sort.combine(&global, local)?;                        // once per thread
//! let source = sort.finalize(global)?;                  // None: no rows
//! let mut scan = source.local_state();                  // per thread
//! while source.get_data(&mut out, &mut scan)? { ... }   // sorted chunks, partition by partition
//! ```
//! Each thread sorts its own run; runs are merged in partitions that threads produce in
//! parallel ([`MergerLocalState::partition_index`] orders them). When the memory
//! reservation is exceeded, runs are finalized early and unpinned, so they spill.

pub(crate) mod algorithm;
pub mod encode;
pub(crate) mod key;
mod merger;
mod pdqsort;
mod run;
mod seq;
mod ska_sort;
mod sorter;
mod strategy;
mod vergesort;

use std::sync::Arc;

use rustdb_buffer::BufferManager;
use rustdb_types::LogicalType;

pub use encode::{
    NullOrder, OrderModifiers, OrderType, SortKeyBuffer, create_sort_keys, decode_sort_key, sort_key_type_for,
};
pub use key::SortKeyType;
pub use merger::{DEFAULT_ROW_GROUP_SIZE, MergerLocalState, SortedRunMerger};
pub use run::SortedRun;
pub use sorter::{
    Sort, SortColumn, SortColumnStats, SortConfig, SortGlobalSinkState, SortLocalSinkState, SortSource,
};
pub use strategy::{
    ChunkRow, FullSort, HashedSort, NaturalSort, SortStrategy, StrategyGlobalState, StrategyLocalState,
};

use crate::layout::TupleDataLayout;

/// Where an output column comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColumnSource {
    /// Decoded from ORDER BY column `n` of the key.
    Key(usize),
    /// Gathered from payload column `n`.
    Payload(usize),
}

/// One output column.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OutputColumn {
    pub(crate) source: ColumnSource,
    pub(crate) output: usize,
}

/// Layouts and column mapping shared by the runs and the merger of one sort.
#[derive(Debug)]
pub(crate) struct SortLayouts {
    pub(crate) buffer_manager: BufferManager,
    pub(crate) key_layout: Arc<TupleDataLayout>,
    pub(crate) payload_layout: Option<Arc<TupleDataLayout>>,
    pub(crate) key_type: SortKeyType,
    /// Types of the ORDER BY columns.
    pub(crate) key_types: Vec<LogicalType>,
    pub(crate) modifiers: Vec<OrderModifiers>,
    pub(crate) outputs: Vec<OutputColumn>,
    pub(crate) output_types: Vec<LogicalType>,
}
