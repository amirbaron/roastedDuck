use std::borrow::Cow;
use std::sync::Arc;

use rustdb_ast::StatementType;
use rustdb_common::Result;
use rustdb_execution::operators::CollectedRows;
use rustdb_planner::StatementReturnType;
use rustdb_row::{ColumnDataCollection, ColumnDataScanState};
use rustdb_types::{DataChunk, LogicalType, Value};

/// Where a materialized result keeps its rows (the allocator of DuckDB's result
/// `ColumnDataCollection`).
#[derive(Clone, Debug)]
enum ResultRows {
    /// Chunks on the heap (`QueryResultMemoryType::IN_MEMORY`).
    InMemory(Arc<[DataChunk]>),
    /// A buffer-managed collection (`QueryResultMemoryType::BUFFER_MANAGED`): counted
    /// against the memory limit and spilled when evicted; chunks are read from it on demand.
    BufferManaged(Arc<ColumnDataCollection>),
}

/// A fully materialised query result (DuckDB's `MaterializedQueryResult`).
#[derive(Clone, Debug)]
pub struct MaterializedResult {
    names: Vec<String>,
    types: Vec<LogicalType>,
    rows: ResultRows,
    /// Row index of the first row of every chunk, plus the row count.
    chunk_starts: Arc<[usize]>,
    /// What the statement returns: rows, a changed-row count or nothing.
    return_type: StatementReturnType,
    /// The kind of statement that produced the result (`QueryResult::statement_type`).
    statement_type: StatementType,
}

impl Default for MaterializedResult {
    /// An empty result without columns.
    fn default() -> Self {
        Self::new(Vec::new(), Vec::new(), Vec::new())
    }
}

fn chunk_starts(counts: impl Iterator<Item = usize>) -> Arc<[usize]> {
    let mut starts = vec![0];
    let mut total = 0;
    for count in counts {
        total += count;
        starts.push(total);
    }
    starts.into()
}

impl MaterializedResult {
    /// A result of in-memory chunks.
    pub fn new(names: Vec<String>, types: Vec<LogicalType>, chunks: Vec<DataChunk>) -> Self {
        let chunk_starts = chunk_starts(chunks.iter().map(DataChunk::len));
        Self {
            names,
            types,
            rows: ResultRows::InMemory(chunks.into()),
            chunk_starts,
            return_type: StatementReturnType::QueryResult,
            statement_type: StatementType::InvalidStatement,
        }
    }

    /// A result of the rows a result collector collected.
    pub(crate) fn from_rows(names: Vec<String>, types: Vec<LogicalType>, rows: CollectedRows) -> Self {
        match rows {
            CollectedRows::Chunks(chunks) => Self::new(names, types, chunks),
            CollectedRows::Collection(collection) => {
                let chunk_starts = chunk_starts(collection.chunk_row_counts());
                Self {
                    names,
                    types,
                    rows: ResultRows::BufferManaged(Arc::new(collection)),
                    chunk_starts,
                    return_type: StatementReturnType::QueryResult,
                    statement_type: StatementType::InvalidStatement,
                }
            }
        }
    }

    /// The result with `return_type` (DuckDB's `StatementProperties::return_type`).
    #[must_use]
    pub fn with_return_type(mut self, return_type: StatementReturnType) -> Self {
        self.return_type = return_type;
        self
    }

    /// The result of a statement of type `statement_type`.
    #[must_use]
    pub fn with_statement_type(mut self, statement_type: StatementType) -> Self {
        self.statement_type = statement_type;
        self
    }

    /// The kind of statement that produced the result.
    pub fn statement_type(&self) -> StatementType {
        self.statement_type
    }

    /// What the statement returns; shells print only query results.
    pub fn return_type(&self) -> StatementReturnType {
        self.return_type
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn types(&self) -> &[LogicalType] {
        &self.types
    }

    pub fn column_count(&self) -> usize {
        self.types.len()
    }

    pub fn row_count(&self) -> usize {
        self.chunk_starts.last().copied().unwrap_or(0)
    }

    /// Whether the rows are in a buffer-managed collection.
    pub fn is_buffer_managed(&self) -> bool {
        matches!(self.rows, ResultRows::BufferManaged(_))
    }

    /// The number of chunks.
    pub fn chunk_count(&self) -> usize {
        self.chunk_starts.len() - 1
    }

    /// The number of rows of chunk `index`.
    pub fn chunk_len(&self, index: usize) -> usize {
        self.chunk_starts[index + 1] - self.chunk_starts[index]
    }

    /// Chunk `index` (`ColumnDataCollection::FetchChunk`): borrowed from an in-memory
    /// result, read from a buffer-managed one.
    pub fn chunk(&self, index: usize) -> Result<Cow<'_, DataChunk>> {
        match &self.rows {
            ResultRows::InMemory(chunks) => Ok(Cow::Borrowed(&chunks[index])),
            ResultRows::BufferManaged(collection) => {
                let mut chunk = DataChunk::default();
                let columns: Vec<usize> = (0..self.types.len()).collect();
                collection.fetch_chunk(&mut ColumnDataScanState::default(), index, &columns, &mut chunk)?;
                Ok(Cow::Owned(chunk))
            }
        }
    }

    /// The chunks in order (`ColumnDataCollection::Chunks`).
    pub fn chunks(&self) -> ResultChunks<'_> {
        ResultChunks {
            result: self,
            index: 0,
            scan: match &self.rows {
                ResultRows::InMemory(_) => None,
                ResultRows::BufferManaged(collection) => Some(collection.initialize_scan()),
            },
        }
    }

    /// The chunks of an in-memory result.
    pub fn in_memory_chunks(&self) -> Option<&[DataChunk]> {
        match &self.rows {
            ResultRows::InMemory(chunks) => Some(chunks),
            ResultRows::BufferManaged(_) => None,
        }
    }

    /// Every chunk in memory: those of an in-memory result, or copies read from a
    /// buffer-managed one.
    pub fn collect_chunks(&self) -> Result<Vec<DataChunk>> {
        match &self.rows {
            ResultRows::InMemory(chunks) => Ok(chunks.to_vec()),
            ResultRows::BufferManaged(_) => self.chunks().map(|chunk| chunk.map(Cow::into_owned)).collect(),
        }
    }

    /// The value at `(column, row)`. For a buffer-managed result this reads the chunk
    /// holding the row; read whole results with [`MaterializedResult::chunks`].
    pub fn value(&self, column: usize, row: usize) -> Value {
        let chunk = self.chunk_starts.partition_point(|&start| start <= row) - 1;
        let offset = row - self.chunk_starts[chunk];
        match self.chunk(chunk) {
            Ok(data) => data.column(column).get_value(offset),
            Err(_) => Value::null(self.types[column].clone()),
        }
    }
}

/// The chunks of a result in order (DuckDB's `ColumnDataChunkIterationHelper`).
#[derive(Debug)]
pub struct ResultChunks<'a> {
    result: &'a MaterializedResult,
    index: usize,
    scan: Option<ColumnDataScanState>,
}

impl<'a> Iterator for ResultChunks<'a> {
    type Item = Result<Cow<'a, DataChunk>>;

    fn next(&mut self) -> Option<Self::Item> {
        match &self.result.rows {
            ResultRows::InMemory(chunks) => {
                let chunk = chunks.get(self.index)?;
                self.index += 1;
                Some(Ok(Cow::Borrowed(chunk)))
            }
            ResultRows::BufferManaged(collection) => {
                let scan = self.scan.as_mut()?;
                collection
                    .scan(scan)
                    .transpose()
                    .map(|chunk| chunk.map(Cow::Owned))
            }
        }
    }
}
