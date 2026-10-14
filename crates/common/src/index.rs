//! Typed column indexes (DuckDB's `LogicalIndex` / `PhysicalIndex`).

use std::fmt;

use crate::Idx;

/// Index of a column among all columns of a table, generated ones included.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LogicalIndex(pub Idx);

/// Index of a column among the physically stored columns of a table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PhysicalIndex(pub Idx);

impl fmt::Display for LogicalIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for PhysicalIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
