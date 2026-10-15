use rustdb_planner::StatementReturnType;
use rustdb_types::{DataChunk, LogicalType, Value};

/// A fully materialised query result (DuckDB's `MaterializedQueryResult`).
#[derive(Clone, Debug, Default)]
pub struct MaterializedResult {
    names: Vec<String>,
    types: Vec<LogicalType>,
    chunks: Vec<DataChunk>,
    /// `(chunk index, row in chunk)` for each result row.
    row_index: Vec<(u32, u32)>,
    /// What the statement returns: rows, a changed-row count or nothing.
    return_type: StatementReturnType,
}

impl MaterializedResult {
    pub fn new(names: Vec<String>, types: Vec<LogicalType>, chunks: Vec<DataChunk>) -> Self {
        let row_index = chunks
            .iter()
            .enumerate()
            .flat_map(|(c, chunk)| (0..chunk.len()).map(move |r| (c as u32, r as u32)))
            .collect();
        Self {
            names,
            types,
            chunks,
            row_index,
            return_type: StatementReturnType::QueryResult,
        }
    }

    /// The result with `return_type` (DuckDB's `StatementProperties::return_type`).
    #[must_use]
    pub fn with_return_type(mut self, return_type: StatementReturnType) -> Self {
        self.return_type = return_type;
        self
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
        self.row_index.len()
    }

    pub fn chunks(&self) -> &[DataChunk] {
        &self.chunks
    }

    /// The value at `(column, row)`.
    pub fn value(&self, column: usize, row: usize) -> Value {
        let (chunk, row) = self.row_index[row];
        self.chunks[chunk as usize].column(column).get_value(row as usize)
    }
}
