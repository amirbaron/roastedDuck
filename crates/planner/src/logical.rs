use std::sync::Arc;

use rustdb_types::{DataChunk, LogicalType};

use crate::{ColumnBinding, Expression};

/// The LIMIT or OFFSET of a query (DuckDB's `BoundLimitNode`).
#[derive(Clone, Debug, Default)]
pub enum BoundLimit {
    #[default]
    Unset,
    Constant(u64),
    /// `LIMIT 10%`
    Percentage(f64),
    Expression(Box<Expression>),
    ExpressionPercentage(Box<Expression>),
}

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
    /// Grouped or ungrouped aggregation; `expressions` holds the aggregates. Outputs the
    /// groups (bound at `group_index`) followed by the aggregates (at `aggregate_index`).
    Aggregate {
        group_index: u64,
        aggregate_index: u64,
        groupings_index: u64,
        groups: Vec<Expression>,
    },
    /// LIMIT/OFFSET.
    Limit { limit: BoundLimit, offset: BoundLimit },
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
        let table = |table_index: u64, count: usize| {
            (0..count as u64).map(move |column_index| ColumnBinding {
                table_index,
                column_index,
            })
        };
        match &self.kind {
            LogicalKind::DummyScan { table_index } => table(*table_index, 1).collect(),
            LogicalKind::Projection { table_index } => table(*table_index, self.expressions.len()).collect(),
            LogicalKind::ColumnDataGet {
                table_index, types, ..
            } => table(*table_index, types.len()).collect(),
            LogicalKind::Aggregate {
                group_index,
                aggregate_index,
                groups,
                ..
            } => table(*group_index, groups.len())
                .chain(table(*aggregate_index, self.expressions.len()))
                .collect(),
            LogicalKind::Filter | LogicalKind::Limit { .. } => self
                .children
                .first()
                .map(Self::column_bindings)
                .unwrap_or_default(),
        }
    }

    /// Recomputes `types` from the children and expressions.
    pub fn resolve_types(&mut self) {
        self.types = match &self.kind {
            LogicalKind::DummyScan { .. } => vec![LogicalType::INTEGER],
            LogicalKind::Projection { .. } => self.expressions.iter().map(Expression::return_type).collect(),
            LogicalKind::Filter => self.children.first().map(|c| c.types.clone()).unwrap_or_default(),
            LogicalKind::ColumnDataGet { types, .. } => types.clone(),
            LogicalKind::Aggregate { groups, .. } => groups
                .iter()
                .chain(&self.expressions)
                .map(Expression::return_type)
                .collect(),
            LogicalKind::Limit { .. } => self.children.first().map(|c| c.types.clone()).unwrap_or_default(),
        };
    }
}
