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

    pub fn into_columns(self) -> Vec<Vector> {
        self.columns
    }
}
