use crate::{LogicalType, Vector};

/// A horizontal slice of a table: one vector per column, all with `len` rows.
#[derive(Clone, Debug, Default)]
pub struct DataChunk {
    columns: Vec<Vector>,
    len: usize,
}

impl DataChunk {
    pub fn new(columns: Vec<Vector>, len: usize) -> Self {
        Self { columns, len }
    }

    /// Empty flat vectors for `types`, each with room for `capacity` rows.
    pub fn with_types(types: &[LogicalType], capacity: usize) -> Self {
        Self {
            columns: types
                .iter()
                .map(|ty| Vector::flat(ty.clone(), capacity))
                .collect(),
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Empties the chunk, keeping its column types.
    pub fn reset(&mut self) {
        self.len = 0;
    }

    /// Replaces all columns at once.
    pub fn set_columns(&mut self, columns: Vec<Vector>, len: usize) {
        self.columns = columns;
        self.len = len;
    }

    /// Empties the chunk and returns its columns, whose list can be refilled and handed
    /// back with [`DataChunk::set_columns`] without allocating a new one.
    pub fn take_columns(&mut self) -> Vec<Vector> {
        self.len = 0;
        std::mem::take(&mut self.columns)
    }

    pub fn set_len(&mut self, len: usize) {
        self.len = len;
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    pub fn column(&self, index: usize) -> &Vector {
        &self.columns[index]
    }

    pub fn column_mut(&mut self, index: usize) -> &mut Vector {
        &mut self.columns[index]
    }

    pub fn columns(&self) -> &[Vector] {
        &self.columns
    }

    /// The bytes the chunk's rows occupy (DuckDB's `DataChunk::GetAllocationSize`).
    pub fn allocation_size(&self) -> usize {
        self.columns
            .iter()
            .map(|column| column.allocation_size(self.len))
            .sum()
    }

    pub fn into_columns(self) -> Vec<Vector> {
        self.columns
    }

    /// The chunk's rows in vectors that hold nothing else (see [`Vector::compacted`]),
    /// for operators that keep chunks after the pipeline moved on.
    #[must_use]
    pub fn compacted(&self) -> Self {
        Self {
            columns: self
                .columns
                .iter()
                .map(|column| column.compacted(self.len))
                .collect(),
            len: self.len,
        }
    }
}
