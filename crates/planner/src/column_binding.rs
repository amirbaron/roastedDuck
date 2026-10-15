use std::fmt;

/// Identifies a column produced by a logical operator: `(table index, column index)`
/// (DuckDB's `ColumnBinding`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ColumnBinding {
    pub table_index: u64,
    pub column_index: u64,
}

impl ColumnBinding {
    pub const fn new(table_index: u64, column_index: u64) -> Self {
        Self {
            table_index,
            column_index,
        }
    }
}

impl fmt::Display for ColumnBinding {
    /// DuckDB's `ColumnBinding::ToString`: `#[table.column]`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#[{}.{}]", self.table_index, self.column_index)
    }
}

/// The bindings `(table_index, 0..count)` (DuckDB's `LogicalOperator::GenerateColumnBindings`).
pub fn generate_column_bindings(table_index: u64, count: usize) -> Vec<ColumnBinding> {
    (0..count as u64)
        .map(|column_index| ColumnBinding::new(table_index, column_index))
        .collect()
}
