//! Materialized chunks for operators that rescan their input (DuckDB's
//! `ColumnDataCollection` as used by the nested loop joins).
//!
//! Rows are kept in a buffer-managed [`TupleDataCollection`], so they count against the
//! memory limit and can spill. Like DuckDB's collection, appended rows are packed into
//! chunks of up to `STANDARD_VECTOR_SIZE` rows, which fixes the chunk boundaries that the
//! nested loop joins' output order depends on.

use std::sync::Arc;

use rustdb_buffer::{BufferManager, MemoryTag};
use rustdb_common::Result;
use rustdb_row::{AppendState, PinProperties, TupleDataCollection, TupleDataLayout, ValidityType};
use rustdb_types::{DataChunk, LogicalType, Vector, VectorKind};

/// Packed, buffer-managed chunks of rows of fixed types; see the module docs.
#[derive(Debug)]
pub struct ColumnDataCollection {
    types: Vec<LogicalType>,
    /// The rows; a placeholder BOOLEAN column stands in when there are no types.
    rows: TupleDataCollection,
    /// Row index of the first row of every chunk, plus the total row count.
    chunk_starts: Vec<usize>,
}

impl ColumnDataCollection {
    /// An empty collection of rows of `types`.
    pub fn new(buffer_manager: BufferManager, types: Vec<LogicalType>) -> Result<Self> {
        let layout_types = if types.is_empty() {
            vec![LogicalType::BOOLEAN]
        } else {
            types.clone()
        };
        let layout = Arc::new(TupleDataLayout::new(layout_types, ValidityType::CanHaveNull)?);
        Ok(Self {
            types,
            rows: TupleDataCollection::new(buffer_manager, layout, MemoryTag::ColumnData)?,
            chunk_starts: vec![0],
        })
    }

    /// A state for appending chunks.
    pub fn initialize_append(&mut self) -> AppendState {
        self.rows.initialize_append(PinProperties::UnpinAfterDone)
    }

    /// Appends the rows of `chunk`.
    pub fn append(&mut self, state: &mut AppendState, chunk: &DataChunk) -> Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        let columns: Vec<Vector> = if self.types.is_empty() {
            vec![Vector::flat(LogicalType::BOOLEAN, chunk.len())]
        } else {
            chunk
                .columns()
                .iter()
                .map(|column| {
                    if column.kind() == VectorKind::Sequence {
                        column.flattened(chunk.len())
                    } else {
                        column.clone()
                    }
                })
                .collect()
        };
        self.rows.append(state, &DataChunk::new(columns, chunk.len()))
    }

    /// Releases the pins of an append and indexes the chunks; call before scanning.
    pub fn finalize_append(&mut self, state: &mut AppendState) {
        self.rows.finalize_pin_state(&mut state.pin_state);
        self.chunk_starts = vec![0];
        let mut total = 0;
        for count in self.rows.chunk_row_counts() {
            total += count;
            self.chunk_starts.push(total);
        }
    }

    /// Number of rows.
    pub fn count(&self) -> usize {
        self.rows.count()
    }

    /// Number of chunks.
    pub fn chunk_count(&self) -> usize {
        self.chunk_starts.len() - 1
    }

    /// Index of the first row of chunk `index`.
    pub fn chunk_start(&self, index: usize) -> usize {
        self.chunk_starts[index]
    }

    /// The rows, for reading them back chunk by chunk like
    /// [`ColumnDataCollection::fetch_chunk`] does (call after `finalize_append`).
    pub fn into_rows(self) -> TupleDataCollection {
        self.rows
    }

    /// Reads columns `columns` of chunk `index` into `result`.
    pub fn fetch_chunk(&self, index: usize, columns: &[usize], result: &mut DataChunk) -> Result<()> {
        if self.types.is_empty() {
            let len = self.chunk_starts[index + 1] - self.chunk_starts[index];
            result.set_columns(Vec::new(), len);
            return Ok(());
        }
        let mut scan = self
            .rows
            .initialize_scan_columns(columns.to_vec(), PinProperties::UnpinAfterDone);
        self.rows.seek(&mut scan, index);
        self.rows.scan(&mut scan, result)?;
        Ok(())
    }
}
