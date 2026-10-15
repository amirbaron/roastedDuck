//! Logical operators (DuckDB's `LogicalOperator` hierarchy in `planner/operator`).
//!
//! A [`LogicalOperator`] holds the members every operator has (children, expressions,
//! output types, cardinality estimate) and its operator-specific data in [`LogicalKind`].
//! The generic members let plan rewrites traverse any operator uniformly.

mod display;

use std::sync::Arc;

use rustdb_ast::{
    CteMaterialize, ExplainFormat, ExplainType, ExpressionType, JoinType, SampleOptions, SetScope,
    TransactionInfo,
};
use rustdb_common::{Error, Result};
use rustdb_function::{FunctionData, TableFunction};
use rustdb_types::{DataChunk, LogicalType, Value};

use crate::binder::CorrelatedColumns;
use crate::column_binding::generate_column_bindings;
use crate::expression::{BoundOrderByNode, BoundOrderModifier, Expression};
use crate::generated::enums::{DistinctType, LogicalOperatorType};
use crate::prepared::PreparedStatementData;
use crate::{ColumnBinding, SubqueryType};

pub use display::render_plan;

/// A LIMIT or OFFSET value (DuckDB's `BoundLimitNode`).
#[derive(Clone, Debug, Default)]
pub enum BoundLimitNode {
    #[default]
    Unset,
    /// `LIMIT 10`
    ConstantValue(u64),
    /// `LIMIT 10%`
    ConstantPercentage(f64),
    /// `LIMIT (expr)`
    ExpressionValue(Box<Expression>),
    /// `LIMIT (expr)%`
    ExpressionPercentage(Box<Expression>),
}

impl BoundLimitNode {
    /// The expression of a non-constant limit.
    pub fn expression_mut(&mut self) -> Option<&mut Expression> {
        match self {
            Self::ExpressionValue(expression) | Self::ExpressionPercentage(expression) => Some(expression),
            Self::Unset | Self::ConstantValue(_) | Self::ConstantPercentage(_) => None,
        }
    }

    /// The expression of a non-constant limit.
    pub fn expression(&self) -> Option<&Expression> {
        match self {
            Self::ExpressionValue(expression) | Self::ExpressionPercentage(expression) => Some(expression),
            Self::Unset | Self::ConstantValue(_) | Self::ConstantPercentage(_) => None,
        }
    }
}

/// A set of group indexes (DuckDB's `GroupingSet`).
pub type GroupingSet = std::collections::BTreeSet<u64>;

/// A column of a table or table function, possibly a field path into a nested column
/// (DuckDB's `ColumnIndex`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ColumnIndex {
    pub index: u64,
    pub child_indexes: Vec<ColumnIndex>,
}

/// The pseudo column id of `rowid` (DuckDB's `COLUMN_IDENTIFIER_ROW_ID`).
pub const COLUMN_IDENTIFIER_ROW_ID: u64 = u64::MAX;

impl ColumnIndex {
    pub const fn new(index: u64) -> Self {
        Self {
            index,
            child_indexes: Vec::new(),
        }
    }

    pub const fn is_row_id_column(&self) -> bool {
        self.index == COLUMN_IDENTIFIER_ROW_ID
    }
}

/// `GROUP BY` aggregation (DuckDB's `LogicalAggregate`). The aggregates are the
/// operator's `expressions`.
#[derive(Clone, Debug, Default)]
pub struct LogicalAggregate {
    pub group_index: u64,
    pub aggregate_index: u64,
    pub groupings_index: u64,
    pub groups: Vec<Expression>,
    pub grouping_sets: Vec<GroupingSet>,
    /// For each `GROUPING(...)` call: the group indexes it inspects.
    pub grouping_functions: Vec<Vec<u64>>,
}

/// A table or table function scan (DuckDB's `LogicalGet`).
#[derive(Clone, Debug)]
pub struct LogicalGet {
    pub table_index: u64,
    pub function: TableFunction,
    pub bind_data: Option<Arc<dyn FunctionData>>,
    /// All columns the function can produce.
    pub returned_types: Vec<LogicalType>,
    pub names: Vec<String>,
    /// The columns actually scanned (indexes into `returned_types`).
    pub column_ids: Vec<ColumnIndex>,
    /// The scanned columns that are output (empty: all of them).
    pub projection_ids: Vec<u64>,
    pub parameters: Vec<Value>,
    pub named_parameters: Vec<(String, Value)>,
    pub input_table_types: Vec<LogicalType>,
    pub input_table_names: Vec<String>,
    /// Input columns of a table-in/table-out function passed through to the output.
    pub projected_input: Vec<u64>,
    /// The column holding `WITH ORDINALITY` numbers.
    pub ordinality_index: Option<u64>,
}

impl LogicalGet {
    /// The type of a scanned column.
    pub fn column_type(&self, column: &ColumnIndex) -> LogicalType {
        if column.is_row_id_column() {
            return LogicalType::BIGINT;
        }
        self.returned_types
            .get(column.index as usize)
            .cloned()
            .unwrap_or(LogicalType::BIGINT)
    }
}

/// A join on comparisons between the two sides (DuckDB's `JoinCondition`).
#[derive(Clone, Debug)]
pub struct JoinCondition {
    pub left: Expression,
    pub right: Expression,
    pub comparison: ExpressionType,
}

/// Equi/range joins, also the delim and as-of joins (DuckDB's `LogicalComparisonJoin`).
#[derive(Clone, Debug)]
pub struct LogicalComparisonJoin {
    /// `LOGICAL_COMPARISON_JOIN`, `LOGICAL_DELIM_JOIN` or `LOGICAL_ASOF_JOIN`.
    pub operator_type: LogicalOperatorType,
    pub join_type: JoinType,
    /// The table index of the boolean column of a MARK join.
    pub mark_index: u64,
    pub left_projection_map: Vec<u64>,
    pub right_projection_map: Vec<u64>,
    pub conditions: Vec<JoinCondition>,
    pub mark_types: Vec<LogicalType>,
    /// Delim joins: the columns whose distinct values are passed to the right side.
    pub duplicate_eliminated_columns: Vec<Expression>,
    pub delim_flipped: bool,
    /// Extra non-comparison predicate.
    pub predicate: Option<Expression>,
}

impl JoinCondition {
    /// The condition as a comparison (DuckDB's `JoinCondition::CreateExpression`).
    pub fn into_expression(self) -> Expression {
        Expression::comparison(self.comparison, self.left, self.right)
    }
}

impl LogicalComparisonJoin {
    /// A join without conditions (DuckDB's `LogicalComparisonJoin(type, logical_type)`).
    pub fn new(join_type: JoinType, operator_type: LogicalOperatorType) -> Self {
        Self {
            operator_type,
            join_type,
            mark_index: 0,
            left_projection_map: Vec::new(),
            right_projection_map: Vec::new(),
            conditions: Vec::new(),
            mark_types: Vec::new(),
            duplicate_eliminated_columns: Vec::new(),
            delim_flipped: false,
            predicate: None,
        }
    }
}

impl LogicalDependentJoin {
    /// DuckDB's `LogicalDependentJoin(JoinType)`.
    pub fn new(join_type: JoinType) -> Self {
        Self {
            join: LogicalComparisonJoin::new(join_type, LogicalOperatorType::LogicalDependentJoin),
            join_condition: None,
            correlated_columns: CorrelatedColumns::default(),
            subquery_type: SubqueryType::Invalid,
            perform_delim: true,
            any_join: false,
            propagate_null_values: true,
            is_lateral_join: false,
            arbitrary_expressions: Vec::new(),
            expression_children: Vec::new(),
            child_types: Vec::new(),
            child_targets: Vec::new(),
            comparison_type: ExpressionType::Invalid,
        }
    }
}

/// A dependent (correlated) join to be decorrelated (DuckDB's `LogicalDependentJoin`,
/// which extends `LogicalComparisonJoin`: `join` holds the members it inherits).
#[derive(Clone, Debug)]
pub struct LogicalDependentJoin {
    pub join: LogicalComparisonJoin,
    pub join_condition: Option<Expression>,
    pub correlated_columns: CorrelatedColumns,
    pub subquery_type: SubqueryType,
    pub perform_delim: bool,
    pub any_join: bool,
    pub propagate_null_values: bool,
    pub is_lateral_join: bool,
    pub arbitrary_expressions: Vec<Expression>,
    pub expression_children: Vec<Expression>,
    pub child_types: Vec<LogicalType>,
    pub child_targets: Vec<LogicalType>,
    pub comparison_type: ExpressionType,
}

/// Operator-specific data of a logical operator.
#[derive(Clone, Debug)]
pub enum LogicalKind {
    /// Computes `expressions` for each input row.
    Projection { table_index: u64 },
    /// Keeps rows where all `expressions` are true.
    Filter { projection_map: Vec<u64> },
    Aggregate(Box<LogicalAggregate>),
    /// Window functions (`expressions`) computed over the input.
    Window { window_index: u64 },
    /// `UNNEST` of list `expressions`.
    Unnest { unnest_index: u64 },
    Limit { limit: BoundLimitNode, offset: BoundLimitNode },
    Order { orders: Vec<BoundOrderByNode>, projection_map: Vec<u64> },
    TopN { orders: Vec<BoundOrderByNode>, limit: u64, offset: u64 },
    Distinct {
        distinct_type: DistinctType,
        distinct_targets: Vec<Expression>,
        order_by: Option<BoundOrderModifier>,
    },
    Sample { sample_options: Box<SampleOptions> },
    Pivot { pivot_index: u64, bound_pivot: Box<BoundPivotInfo> },
    Get(Box<LogicalGet>),
    /// Scans materialised data (DuckDB's `LogicalColumnDataGet`).
    ChunkGet {
        table_index: u64,
        chunk_types: Vec<LogicalType>,
        collection: Arc<[DataChunk]>,
    },
    /// The duplicate-eliminated input of a delim join.
    DelimGet { table_index: u64, chunk_types: Vec<LogicalType> },
    /// Rows of expressions (`VALUES`), evaluated against a child row.
    ExpressionGet {
        table_index: u64,
        expr_types: Vec<LogicalType>,
        expressions: Vec<Vec<Expression>>,
    },
    /// A single row with no columns (`SELECT 42`).
    DummyScan { table_index: u64 },
    /// No rows at all.
    EmptyResult {
        return_types: Vec<LogicalType>,
        bindings: Vec<ColumnBinding>,
    },
    /// A reference to a CTE's result.
    CteRef {
        table_index: u64,
        cte_index: u64,
        chunk_types: Vec<LogicalType>,
        bound_columns: Vec<String>,
        correlated_columns: u64,
        is_recurring: bool,
    },
    ComparisonJoin(Box<LogicalComparisonJoin>),
    /// A join on an arbitrary condition.
    AnyJoin { join_type: JoinType, condition: Box<Expression> },
    CrossProduct,
    PositionalJoin,
    DependentJoin(Box<LogicalDependentJoin>),
    /// `UNION`, `EXCEPT`, `INTERSECT` (the type is in `operator_type`).
    SetOperation {
        operator_type: LogicalOperatorType,
        table_index: u64,
        column_count: u64,
        setop_all: bool,
        allow_out_of_order: bool,
    },
    RecursiveCte {
        ctename: String,
        table_index: u64,
        column_count: u64,
        union_all: bool,
        key_targets: Vec<Expression>,
        correlated_columns: CorrelatedColumns,
    },
    MaterializedCte {
        ctename: String,
        table_index: u64,
        column_count: u64,
        materialize: CteMaterialize,
        correlated_columns: CorrelatedColumns,
    },
    Explain {
        explain_type: ExplainType,
        explain_format: ExplainFormat,
    },
    Prepare { name: String, prepared: Arc<PreparedStatementData> },
    Execute { prepared: Arc<PreparedStatementData> },
    /// `SET name = value`.
    Set { name: String, value: Value, scope: SetScope },
    /// `RESET name`.
    Reset { name: String, scope: SetScope },
    /// `BEGIN` / `COMMIT` / `ROLLBACK`.
    Transaction { info: TransactionInfo },
    /// `DEALLOCATE name`.
    Deallocate { name: String },
    /// A `PRAGMA` call without a result.
    Pragma { name: String, parameters: Vec<Value> },
}

/// What a `PIVOT` computes (DuckDB's `BoundPivotInfo`).
#[derive(Clone, Debug, Default)]
pub struct BoundPivotInfo {
    pub group_count: u64,
    pub types: Vec<LogicalType>,
    pub pivot_values: Vec<String>,
    pub aggregates: Vec<Expression>,
}

/// A node of a logical plan (DuckDB's `LogicalOperator`).
#[derive(Clone, Debug)]
pub struct LogicalOperator {
    pub kind: LogicalKind,
    pub children: Vec<LogicalOperator>,
    pub expressions: Vec<Expression>,
    /// Output column types, filled by [`LogicalOperator::resolve_operator_types`].
    pub types: Vec<LogicalType>,
    pub estimated_cardinality: Option<u64>,
}

fn map_list<T: Clone>(items: Vec<T>, projection_map: &[u64]) -> Vec<T> {
    if projection_map.is_empty() {
        return items;
    }
    projection_map
        .iter()
        .filter_map(|&index| items.get(index as usize).cloned())
        .collect()
}

impl LogicalOperator {
    /// An operator without children or expressions.
    pub fn new(kind: LogicalKind) -> Self {
        Self {
            kind,
            children: Vec::new(),
            expressions: Vec::new(),
            types: Vec::new(),
            estimated_cardinality: None,
        }
    }

    /// An operator with the given expressions.
    #[must_use]
    pub fn with_expressions(mut self, expressions: Vec<Expression>) -> Self {
        self.expressions = expressions;
        self
    }

    /// An operator with the given children.
    #[must_use]
    pub fn with_children(mut self, children: Vec<Self>) -> Self {
        self.children = children;
        self
    }

    /// Appends a child (DuckDB's `AddChild`).
    pub fn add_child(&mut self, child: Self) {
        self.children.push(child);
    }

    /// `LOGICAL_PROJECTION` over `child`.
    pub fn projection(table_index: u64, expressions: Vec<Expression>, child: Self) -> Self {
        Self::new(LogicalKind::Projection { table_index })
            .with_expressions(expressions)
            .with_children(vec![child])
    }

    /// `LOGICAL_FILTER` with one predicate over `child`.
    pub fn filter(predicate: Expression, child: Self) -> Self {
        Self::new(LogicalKind::Filter {
            projection_map: Vec::new(),
        })
        .with_expressions(vec![predicate])
        .with_children(vec![child])
    }

    /// `LOGICAL_CROSS_PRODUCT` of two inputs.
    pub fn cross_product(left: Self, right: Self) -> Self {
        Self::new(LogicalKind::CrossProduct).with_children(vec![left, right])
    }

    /// The operator's type (DuckDB's `LogicalOperator::type`).
    pub fn operator_type(&self) -> LogicalOperatorType {
        use LogicalOperatorType as T;
        match &self.kind {
            LogicalKind::Projection { .. } => T::LogicalProjection,
            LogicalKind::Filter { .. } => T::LogicalFilter,
            LogicalKind::Aggregate(_) => T::LogicalAggregateAndGroupBy,
            LogicalKind::Window { .. } => T::LogicalWindow,
            LogicalKind::Unnest { .. } => T::LogicalUnnest,
            LogicalKind::Limit { .. } => T::LogicalLimit,
            LogicalKind::Order { .. } => T::LogicalOrderBy,
            LogicalKind::TopN { .. } => T::LogicalTopN,
            LogicalKind::Distinct { .. } => T::LogicalDistinct,
            LogicalKind::Sample { .. } => T::LogicalSample,
            LogicalKind::Pivot { .. } => T::LogicalPivot,
            LogicalKind::Get(_) => T::LogicalGet,
            LogicalKind::ChunkGet { .. } => T::LogicalChunkGet,
            LogicalKind::DelimGet { .. } => T::LogicalDelimGet,
            LogicalKind::ExpressionGet { .. } => T::LogicalExpressionGet,
            LogicalKind::DummyScan { .. } => T::LogicalDummyScan,
            LogicalKind::EmptyResult { .. } => T::LogicalEmptyResult,
            LogicalKind::CteRef { .. } => T::LogicalCteRef,
            LogicalKind::ComparisonJoin(join) => join.operator_type,
            LogicalKind::AnyJoin { .. } => T::LogicalAnyJoin,
            LogicalKind::CrossProduct => T::LogicalCrossProduct,
            LogicalKind::PositionalJoin => T::LogicalPositionalJoin,
            LogicalKind::DependentJoin(_) => T::LogicalDependentJoin,
            LogicalKind::SetOperation { operator_type, .. } => *operator_type,
            LogicalKind::RecursiveCte { .. } => T::LogicalRecursiveCte,
            LogicalKind::MaterializedCte { .. } => T::LogicalMaterializedCte,
            LogicalKind::Explain { .. } => T::LogicalExplain,
            LogicalKind::Prepare { .. } => T::LogicalPrepare,
            LogicalKind::Execute { .. } => T::LogicalExecute,
            LogicalKind::Set { .. } => T::LogicalSet,
            LogicalKind::Reset { .. } => T::LogicalReset,
            LogicalKind::Transaction { .. } => T::LogicalTransaction,
            LogicalKind::Deallocate { .. } => T::LogicalDrop,
            LogicalKind::Pragma { .. } => T::LogicalPragma,
        }
    }

    /// The join type of a join operator.
    pub fn join_type(&self) -> Option<JoinType> {
        match &self.kind {
            LogicalKind::ComparisonJoin(join) => Some(join.join_type),
            LogicalKind::AnyJoin { join_type, .. } => Some(*join_type),
            LogicalKind::DependentJoin(dependent) => Some(dependent.join.join_type),
            _ => None,
        }
    }

    fn child_bindings(&self, index: usize) -> Vec<ColumnBinding> {
        self.children.get(index).map(Self::column_bindings).unwrap_or_default()
    }

    fn child_types(&self, index: usize) -> Vec<LogicalType> {
        self.children.get(index).map(|child| child.types.clone()).unwrap_or_default()
    }

    /// The bindings of the columns this operator outputs (DuckDB's `GetColumnBindings`).
    pub fn column_bindings(&self) -> Vec<ColumnBinding> {
        match &self.kind {
            LogicalKind::Projection { table_index } => generate_column_bindings(*table_index, self.expressions.len()),
            LogicalKind::Filter { projection_map } | LogicalKind::Order { projection_map, .. } => {
                map_list(self.child_bindings(0), projection_map)
            }
            LogicalKind::Aggregate(aggregate) => {
                let mut bindings = generate_column_bindings(aggregate.group_index, aggregate.groups.len());
                bindings.extend(generate_column_bindings(aggregate.aggregate_index, self.expressions.len()));
                bindings.extend(generate_column_bindings(
                    aggregate.groupings_index,
                    aggregate.grouping_functions.len(),
                ));
                bindings
            }
            LogicalKind::Window { window_index: index } | LogicalKind::Unnest { unnest_index: index } => {
                let mut bindings = self.child_bindings(0);
                bindings.extend(generate_column_bindings(*index, self.expressions.len()));
                bindings
            }
            LogicalKind::Limit { .. }
            | LogicalKind::TopN { .. }
            | LogicalKind::Distinct { .. }
            | LogicalKind::Sample { .. } => self.child_bindings(0),
            LogicalKind::Pivot {
                pivot_index,
                bound_pivot,
            } => generate_column_bindings(*pivot_index, bound_pivot.types.len()),
            LogicalKind::Get(get) => {
                if get.column_ids.is_empty() {
                    return vec![ColumnBinding::new(get.table_index, 0)];
                }
                let mut bindings = if get.projection_ids.is_empty() {
                    generate_column_bindings(get.table_index, get.column_ids.len())
                } else {
                    get.projection_ids
                        .iter()
                        .map(|&id| ColumnBinding::new(get.table_index, id))
                        .collect()
                };
                if !get.projected_input.is_empty() {
                    let child = self.child_bindings(0);
                    bindings.extend(
                        get.projected_input
                            .iter()
                            .filter_map(|&entry| child.get(entry as usize).copied()),
                    );
                }
                bindings
            }
            LogicalKind::ChunkGet {
                table_index,
                chunk_types,
                ..
            }
            | LogicalKind::DelimGet {
                table_index,
                chunk_types,
            }
            | LogicalKind::CteRef {
                table_index,
                chunk_types,
                ..
            } => generate_column_bindings(*table_index, chunk_types.len()),
            LogicalKind::ExpressionGet {
                table_index,
                expr_types,
                ..
            } => generate_column_bindings(*table_index, expr_types.len()),
            LogicalKind::DummyScan { table_index } => vec![ColumnBinding::new(*table_index, 0)],
            LogicalKind::EmptyResult { bindings, .. } => bindings.clone(),
            LogicalKind::ComparisonJoin(join) => self.join_bindings(
                join.join_type,
                join.mark_index,
                &join.left_projection_map,
                &join.right_projection_map,
            ),
            LogicalKind::AnyJoin { join_type, .. } => self.join_bindings(*join_type, 0, &[], &[]),
            LogicalKind::DependentJoin(dependent) => self.join_bindings(
                dependent.join.join_type,
                dependent.join.mark_index,
                &dependent.join.left_projection_map,
                &dependent.join.right_projection_map,
            ),
            LogicalKind::CrossProduct | LogicalKind::PositionalJoin => {
                let mut bindings = self.child_bindings(0);
                bindings.extend(self.child_bindings(1));
                bindings
            }
            LogicalKind::SetOperation {
                table_index,
                column_count,
                ..
            }
            | LogicalKind::RecursiveCte {
                table_index,
                column_count,
                ..
            } => generate_column_bindings(*table_index, *column_count as usize),
            LogicalKind::MaterializedCte { .. } => self.child_bindings(1),
            LogicalKind::Explain { .. } => vec![ColumnBinding::new(0, 0), ColumnBinding::new(0, 1)],
            LogicalKind::Execute { prepared } => generate_column_bindings(0, prepared.types.len()),
            LogicalKind::Prepare { .. }
            | LogicalKind::Set { .. }
            | LogicalKind::Reset { .. }
            | LogicalKind::Transaction { .. }
            | LogicalKind::Deallocate { .. }
            | LogicalKind::Pragma { .. } => vec![ColumnBinding::new(0, 0)],
        }
    }

    /// The single table index all output columns share (DuckDB's `GetRootIndex`).
    pub fn root_index(&self) -> Result<u64> {
        let bindings = self.column_bindings();
        let first = bindings
            .first()
            .ok_or_else(|| Error::internal("Empty bindings in GetRootIndex"))?
            .table_index;
        if bindings.iter().any(|binding| binding.table_index != first) {
            return Err(Error::internal("GetRootIndex - multiple column bindings found"));
        }
        Ok(first)
    }

    /// DuckDB's `LogicalJoin::GetColumnBindings`.
    fn join_bindings(
        &self,
        join_type: JoinType,
        mark_index: u64,
        left_projection_map: &[u64],
        right_projection_map: &[u64],
    ) -> Vec<ColumnBinding> {
        let mut left = map_list(self.child_bindings(0), left_projection_map);
        match join_type {
            JoinType::Semi | JoinType::Anti => left,
            JoinType::Mark => {
                left.push(ColumnBinding::new(mark_index, 0));
                left
            }
            _ => {
                let right = map_list(self.child_bindings(1), right_projection_map);
                if matches!(join_type, JoinType::RightSemi | JoinType::RightAnti) {
                    return right;
                }
                left.extend(right);
                left
            }
        }
    }

    /// DuckDB's `LogicalJoin::ResolveTypes`.
    fn join_types(&self, join_type: JoinType, left_map: &[u64], right_map: &[u64]) -> Vec<LogicalType> {
        let mut left = map_list(self.child_types(0), left_map);
        match join_type {
            JoinType::Semi | JoinType::Anti => left,
            JoinType::Mark => {
                left.push(LogicalType::BOOLEAN);
                left
            }
            _ => {
                let right = map_list(self.child_types(1), right_map);
                if matches!(join_type, JoinType::RightSemi | JoinType::RightAnti) {
                    return right;
                }
                left.extend(right);
                left
            }
        }
    }

    /// Computes `types` from the children's types and the expressions (DuckDB's
    /// `ResolveTypes`).
    fn resolve_types(&mut self) -> Result<()> {
        if let LogicalKind::Get(get) = &mut self.kind
            && get.column_ids.is_empty()
        {
            // No projection: scan any column to know how many rows there are.
            get.column_ids.push(ColumnIndex::new(COLUMN_IDENTIFIER_ROW_ID));
        }
        let expression_types = || self.expressions.iter().map(|e| e.return_type.clone()).collect::<Vec<_>>();
        self.types = match &self.kind {


            LogicalKind::Projection { .. } => expression_types(),
            LogicalKind::Filter { projection_map } | LogicalKind::Order { projection_map, .. } => {
                map_list(self.child_types(0), projection_map)
            }
            LogicalKind::Aggregate(aggregate) => {
                let mut types: Vec<LogicalType> = aggregate.groups.iter().map(|g| g.return_type.clone()).collect();
                types.extend(expression_types());
                types.extend(std::iter::repeat_n(LogicalType::BIGINT, aggregate.grouping_functions.len()));
                types
            }
            LogicalKind::Window { .. } | LogicalKind::Unnest { .. } => {
                let mut types = self.child_types(0);
                types.extend(expression_types());
                types
            }
            LogicalKind::Limit { .. }
            | LogicalKind::TopN { .. }
            | LogicalKind::Distinct { .. }
            | LogicalKind::Sample { .. }
            | LogicalKind::SetOperation { .. }
            | LogicalKind::RecursiveCte { .. } => self.child_types(0),
            LogicalKind::MaterializedCte { .. } => self.child_types(1),
            LogicalKind::Pivot { bound_pivot, .. } => bound_pivot.types.clone(),
            LogicalKind::Get(get) => {
                let mut types: Vec<LogicalType> = if get.projection_ids.is_empty() {
                    get.column_ids.iter().map(|column| get.column_type(column)).collect()
                } else {
                    get.projection_ids
                        .iter()
                        .filter_map(|&id| get.column_ids.get(id as usize))
                        .map(|column| get.column_type(column))
                        .collect()
                };
                let input = self.children.first().map(|child| child.types.clone()).unwrap_or_default();
                types.extend(
                    get.projected_input
                        .iter()
                        .filter_map(|&entry| input.get(entry as usize).cloned()),
                );
                types
            }
            LogicalKind::ChunkGet { chunk_types, .. }
            | LogicalKind::DelimGet { chunk_types, .. }
            | LogicalKind::CteRef { chunk_types, .. } => chunk_types.clone(),
            LogicalKind::ExpressionGet { expr_types, .. } => expr_types.clone(),
            LogicalKind::DummyScan { .. } => vec![LogicalType::INTEGER],
            LogicalKind::EmptyResult { return_types, .. } => return_types.clone(),
            LogicalKind::ComparisonJoin(join) => {
                let (join_type, left, right) = (
                    join.join_type,
                    join.left_projection_map.clone(),
                    join.right_projection_map.clone(),
                );
                self.join_types(join_type, &left, &right)
            }
            LogicalKind::AnyJoin { join_type, .. } => {
                let join_type = *join_type;
                self.join_types(join_type, &[], &[])
            }
            LogicalKind::DependentJoin(dependent) => {
                let (join_type, left, right) = (
                    dependent.join.join_type,
                    dependent.join.left_projection_map.clone(),
                    dependent.join.right_projection_map.clone(),
                );
                self.join_types(join_type, &left, &right)
            }
            LogicalKind::CrossProduct | LogicalKind::PositionalJoin => {
                let mut types = self.child_types(0);
                types.extend(self.child_types(1));
                types
            }
            LogicalKind::Explain { .. } => vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
            LogicalKind::Execute { prepared } => prepared.types.clone(),
            LogicalKind::Prepare { .. } | LogicalKind::Deallocate { .. } => vec![LogicalType::BOOLEAN],
            LogicalKind::Set { .. }
            | LogicalKind::Reset { .. }
            | LogicalKind::Transaction { .. }
            | LogicalKind::Pragma { .. } => vec![LogicalType::BOOLEAN],
        };
        Ok(())
    }

    /// Resolves the output types of the whole tree, children first (DuckDB's
    /// `ResolveOperatorTypes`).
    pub fn resolve_operator_types(&mut self) -> Result<()> {
        for child in &mut self.children {
            child.resolve_operator_types()?;
        }
        self.resolve_types()?;
        if self.types.len() != self.column_bindings().len() {
            return Err(Error::internal(format!(
                "{} has {} types but {} column bindings",
                self.operator_type().name(),
                self.types.len(),
                self.column_bindings().len()
            )));
        }
        Ok(())
    }

    /// Calls `callback` on every expression of this operator (not its children), including
    /// expressions stored outside `expressions` (DuckDB's
    /// `LogicalOperatorVisitor::EnumerateExpressions`).
    pub fn try_for_each_expression_mut<E>(
        &mut self,
        mut callback: impl FnMut(&mut Expression) -> Result<(), E>,
    ) -> Result<(), E> {
        match &mut self.kind {
            LogicalKind::ExpressionGet { expressions, .. } => {
                for row in expressions {
                    for expression in row {
                        callback(expression)?;
                    }
                }
            }
            LogicalKind::Order { orders, .. } | LogicalKind::TopN { orders, .. } => {
                for order in orders {
                    callback(&mut order.expression)?;
                }
            }
            LogicalKind::Distinct {
                distinct_targets,
                order_by,
                ..
            } => {
                for target in distinct_targets {
                    callback(target)?;
                }
                if let Some(order_by) = order_by {
                    for order in &mut order_by.orders {
                        callback(&mut order.expression)?;
                    }
                }
            }
            LogicalKind::ComparisonJoin(join) => {
                for target in &mut join.duplicate_eliminated_columns {
                    callback(target)?;
                }
                for condition in &mut join.conditions {
                    callback(&mut condition.left)?;
                    callback(&mut condition.right)?;
                }
                if let Some(predicate) = &mut join.predicate {
                    callback(predicate)?;
                }
            }
            LogicalKind::AnyJoin { condition, .. } => callback(condition)?,
            LogicalKind::DependentJoin(dependent) => {
                for target in &mut dependent.join.duplicate_eliminated_columns {
                    callback(target)?;
                }
                for condition in &mut dependent.join.conditions {
                    callback(&mut condition.left)?;
                    callback(&mut condition.right)?;
                }
                for expression in &mut dependent.arbitrary_expressions {
                    callback(expression)?;
                }
                for expression in &mut dependent.expression_children {
                    callback(expression)?;
                }
            }
            LogicalKind::Limit { limit, offset } => {
                for node in [limit, offset] {
                    if let Some(expression) = node.expression_mut() {
                        callback(expression)?;
                    }
                }
            }
            LogicalKind::Aggregate(aggregate) => {
                for group in &mut aggregate.groups {
                    callback(group)?;
                }
            }
            LogicalKind::RecursiveCte { key_targets, .. } => {
                for target in key_targets {
                    callback(target)?;
                }
            }
            _ => {}
        }
        for expression in &mut self.expressions {
            callback(expression)?;
        }
        Ok(())
    }

    /// Calls `callback` on every expression of this operator.
    pub fn for_each_expression_mut(&mut self, mut callback: impl FnMut(&mut Expression)) {
        let _: Result<(), ()> = self.try_for_each_expression_mut(|expression| {
            callback(expression);
            Ok(())
        });
    }
}
