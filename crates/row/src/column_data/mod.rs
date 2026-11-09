//! Chunks stored column by column in buffer-managed blocks (DuckDB's
//! `common/types/column`: `ColumnDataCollection` with its `BUFFER_MANAGER_ALLOCATOR`, its
//! segments and allocator), as buffer-managed query results
//! (`QueryResultMemoryType::BUFFER_MANAGED`) and the window operator's hash groups
//! (`Sort::MaterializeColumnData`) hold them.
//!
//! Every vector of a chunk is one allocation in a block of the buffer manager with room
//! for `STANDARD_VECTOR_SIZE` values followed by the validity words, laid out as DuckDB
//! lays them out, so a collection takes the blocks DuckDB's takes. Appended chunks are
//! packed into chunks of `STANDARD_VECTOR_SIZE` rows. Long strings are copied into heaps
//! in the same blocks; children of lists and arrays, and the fields of structs, are vectors
//! of their own. The blocks count against the memory limit and are spilled to the
//! temporary directory when evicted.
//!
//! Reading a chunk copies the values out (roastedduck vectors own their buffers; DuckDB
//! references the block memory); strings point into the heap blocks, which the vectors keep
//! pinned for as long as the strings live.

mod allocator;
mod collection;
mod segment;

pub use collection::{ColumnDataAppendState, ColumnDataCollection, ColumnDataScanState};
