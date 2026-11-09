use roastedduck_types::LogicalType;

use crate::operator::{PhysicalOperator, PhysicalRef};

/// `UNION ALL` (DuckDB's `PhysicalUnion`): has no state of its own; the executor runs
/// each input in its own pipeline feeding the operators above and the same sink.
#[derive(Debug)]
pub struct PhysicalUnion {
    types: Vec<LogicalType>,
    children: [PhysicalRef; 2],
}

impl PhysicalUnion {
    pub fn new(types: Vec<LogicalType>, left: PhysicalRef, right: PhysicalRef) -> Self {
        Self {
            types,
            children: [left, right],
        }
    }
}

impl PhysicalOperator for PhysicalUnion {
    fn name(&self) -> &'static str {
        "UNION"
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &self.children
    }
    fn is_union(&self) -> bool {
        true
    }
}
