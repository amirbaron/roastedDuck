use std::sync::Arc;

use rustdb_types::{DataChunk, LogicalType};

use crate::{ColumnBinding, Expression};

/// Operator-specific data of a logical operator (DuckDB's `LogicalOperatorType` + subclass
/// fields). Shared fields live on [`LogicalOperator`].
#[derive(Clone, Debug)]
pub enum LogicalKind {
    /// Produces a single row with no columns (`SELECT 42`).
    DummyScan { table_index: u64 },
    /// Computes `expressions` for each input row.
    Projection { table_index: u64 },
    /// Keeps rows where all `expressions` are true.
    Filter,
    /// Scans materialised data (DuckDB's `LogicalColumnDataGet`).
    ColumnDataGet {
        table_index: u64,
        types: Vec<LogicalType>,
        chunks: Arc<[DataChunk]>,
    },
}

/// A node of a logical plan.
///
/// Mirrors DuckDB's `LogicalOperator`: the generic `children`/`expressions`/`types`
/// fields let optimizer passes traverse any operator uniformly.
#[derive(Clone, Debug)]
pub struct LogicalOperator {
    pub kind: LogicalKind,
    pub children: Vec<LogicalOperator>,
    pub expressions: Vec<Expression>,
    /// Output column types, filled by [`LogicalOperator::resolve_types`].
    pub types: Vec<LogicalType>,
    pub estimated_cardinality: u64,
}

impl LogicalOperator {
    pub fn new(kind: LogicalKind, children: Vec<LogicalOperator>, expressions: Vec<Expression>) -> Self {
        let mut op = Self {
            kind,
            children,
            expressions,
            types: Vec::new(),
            estimated_cardinality: 0,
        };
        op.resolve_types();
        op
    }

    /// The bindings of the columns this operator outputs.
    pub fn column_bindings(&self) -> Vec<ColumnBinding> {
        match &self.kind {
            LogicalKind::DummyScan { table_index } => {
                vec![ColumnBinding {
                    table_index: *table_index,
                    column_index: 0,
                }]
            }
            LogicalKind::Projection { table_index } => (0..self.expressions.len())
                .map(|i| ColumnBinding {
                    table_index: *table_index,
                    column_index: i as u64,
                })
                .collect(),
            LogicalKind::Filter => self
                .children
                .first()
                .map(Self::column_bindings)
                .unwrap_or_default(),
            LogicalKind::ColumnDataGet {
                table_index, types, ..
            } => (0..types.len())
                .map(|i| ColumnBinding {
                    table_index: *table_index,
                    column_index: i as u64,
                })
                .collect(),
        }
    }

    /// Recomputes `types` from the children and expressions.
    pub fn resolve_types(&mut self) {
        self.types = match &self.kind {
            LogicalKind::DummyScan { .. } => vec![LogicalType::INTEGER],
            LogicalKind::Projection { .. } => self.expressions.iter().map(Expression::return_type).collect(),
            LogicalKind::Filter => self.children.first().map(|c| c.types.clone()).unwrap_or_default(),
            LogicalKind::ColumnDataGet { types, .. } => types.clone(),
        };
    }
}
