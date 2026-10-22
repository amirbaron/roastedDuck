//! The rows of a sorted group in order, as the sort strategies hand them out (DuckDB's
//! `HashGroup`, a `ColumnDataCollection`).
//!
//! Columns are stored in a [`ColumnDataCollection`]; groups with a nested column, which
//! it does not store, keep rows in a [`TupleDataCollection`] instead.

use std::sync::Arc;

use rustdb_buffer::{BufferManager, MemoryTag};
use rustdb_common::{Error, Result};
use rustdb_types::{DataChunk, LogicalType};

use crate::collection::TupleDataCollection;
use crate::column_data::{ColumnDataAppendState, ColumnDataCollection, ColumnDataScanState};
use crate::layout::{TupleDataLayout, ValidityType};
use crate::state::{AppendState, PinProperties, ScanState};

/// See the module documentation.
#[derive(Debug)]
pub enum GroupData {
    Columns(ColumnDataCollection),
    Rows(TupleDataCollection),
}

/// An append to a [`GroupData`].
#[derive(Debug)]
pub enum GroupDataAppend {
    Columns(ColumnDataAppendState),
    Rows(AppendState),
}

impl GroupData {
    /// An empty group of columns of `types`, charged to `tag`.
    pub fn new(buffer_manager: BufferManager, types: Vec<LogicalType>, tag: MemoryTag) -> Result<Self> {
        if ColumnDataCollection::supports(&types) {
            return Ok(Self::Columns(ColumnDataCollection::new(
                buffer_manager,
                types,
                tag,
            )?));
        }
        let layout = Arc::new(TupleDataLayout::new(types, ValidityType::CanHaveNull)?);
        Ok(Self::Rows(TupleDataCollection::new(buffer_manager, layout, tag)?))
    }

    pub fn initialize_append(&mut self) -> GroupDataAppend {
        match self {
            Self::Columns(_) => GroupDataAppend::Columns(ColumnDataAppendState::default()),
            Self::Rows(rows) => GroupDataAppend::Rows(rows.initialize_append(PinProperties::UnpinAfterDone)),
        }
    }

    pub fn append(&mut self, state: &mut GroupDataAppend, chunk: &DataChunk) -> Result<()> {
        match (self, state) {
            (Self::Columns(columns), GroupDataAppend::Columns(state)) => columns.append(state, chunk),
            (Self::Rows(rows), GroupDataAppend::Rows(state)) => rows.append(state, chunk),
            _ => Err(Error::internal("group data append of the wrong kind")),
        }
    }

    /// Releases the pins of an append.
    pub fn finalize_append(&self, state: &mut GroupDataAppend) {
        match (self, state) {
            (Self::Columns(columns), GroupDataAppend::Columns(state)) => columns.finalize_append(state),
            (Self::Rows(rows), GroupDataAppend::Rows(state)) => rows.finalize_pin_state(&mut state.pin_state),
            _ => {}
        }
    }

    /// Moves the rows of `other` behind this group's.
    pub fn combine(&mut self, other: Self) -> Result<()> {
        match (self, other) {
            (Self::Columns(columns), Self::Columns(other)) => columns.combine(other),
            (Self::Rows(rows), Self::Rows(other)) => rows.combine(other),
            _ => Err(Error::internal("combining group data of different kinds")),
        }
    }

    /// Number of rows.
    pub fn count(&self) -> usize {
        match self {
            Self::Columns(columns) => columns.count(),
            Self::Rows(rows) => rows.count(),
        }
    }

    /// Number of chunks.
    pub fn chunk_count(&self) -> usize {
        match self {
            Self::Columns(columns) => columns.chunk_count(),
            Self::Rows(rows) => rows.chunk_count(),
        }
    }

    /// The number of rows of every chunk, in order.
    pub fn chunk_row_counts(&self) -> Vec<usize> {
        match self {
            Self::Columns(columns) => columns.chunk_row_counts().collect(),
            Self::Rows(rows) => rows.chunk_row_counts().collect(),
        }
    }

    /// Every chunk with all columns.
    pub fn to_chunks(&self) -> Result<Vec<DataChunk>> {
        let column_count = match self {
            Self::Columns(columns) => columns.types().len(),
            Self::Rows(rows) => rows.types().len(),
        };
        let mut scanner = GroupDataScanner::new(self, (0..column_count).collect(), 0);
        let mut chunks = Vec::with_capacity(self.chunk_count());
        loop {
            let mut chunk = DataChunk::default();
            if !scanner.scan(self, &mut chunk)? {
                return Ok(chunks);
            }
            chunks.push(chunk);
        }
    }
}

/// The blocks a [`GroupDataScanner`] keeps pinned.
#[derive(Debug)]
enum GroupScanState {
    Columns(ColumnDataScanState),
    Rows(Box<ScanState>),
}

/// Reads columns of a [`GroupData`] chunk by chunk from a starting chunk (DuckDB's
/// `WindowCollectionChunkScanner`).
#[derive(Debug)]
pub struct GroupDataScanner {
    columns: Vec<usize>,
    next_chunk: usize,
    state: GroupScanState,
}

impl GroupDataScanner {
    /// Reads columns `columns` of `data` from chunk `begin_chunk` on.
    pub fn new(data: &GroupData, columns: Vec<usize>, begin_chunk: usize) -> Self {
        let state = match data {
            GroupData::Columns(_) => GroupScanState::Columns(ColumnDataScanState::default()),
            GroupData::Rows(rows) => {
                let mut scan = rows.initialize_scan_columns(columns.clone(), PinProperties::UnpinAfterDone);
                rows.seek(&mut scan, begin_chunk);
                GroupScanState::Rows(Box::new(scan))
            }
        };
        Self {
            columns,
            next_chunk: begin_chunk,
            state,
        }
    }

    /// The next chunk; `false` past the end.
    pub fn scan(&mut self, data: &GroupData, chunk: &mut DataChunk) -> Result<bool> {
        match (data, &mut self.state) {
            (GroupData::Columns(columns), GroupScanState::Columns(state)) => {
                if self.next_chunk >= columns.chunk_count() {
                    chunk.set_columns(Vec::new(), 0);
                    return Ok(false);
                }
                columns.fetch_chunk(state, self.next_chunk, &self.columns, chunk)?;
                self.next_chunk += 1;
                Ok(true)
            }
            (GroupData::Rows(rows), GroupScanState::Rows(state)) => {
                self.next_chunk += 1;
                rows.scan(state, chunk)
            }
            _ => Err(Error::internal("group data scanner of the wrong kind")),
        }
    }
}
