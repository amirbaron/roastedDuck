//! Proving that an expression cannot be NULL at an operator's output (DuckDB's
//! `NotNullExpressionAnalyzer`): from filters that reject NULLs, joins that keep a side's
//! rows intact, and table statistics without NULLs.

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_catalog::EntryKind;
use rustdb_planner::operator::COLUMN_IDENTIFIER_ROW_ID;
use rustdb_planner::{ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator};
use rustdb_storage::statistics::BaseStatistics;

/// Proves expressions non-NULL over a plan (DuckDB's `NotNullExpressionAnalyzer`).
#[derive(Debug)]
pub struct NotNullExpressionAnalyzer<'a> {
    /// The plan whose CTEs `CTE_REF`s resolve to, if any.
    plan_root: Option<&'a LogicalOperator>,
}

/// The binding of a column reference of the current query level.
fn column_ref_binding(expression: &Expression) -> Option<ColumnBinding> {
    match &expression.kind {
        ExpressionKind::ColumnRef(column) if column.depth == 0 => Some(column.binding),
        _ => None,
    }
}

/// Whether `filter` is false or NULL whenever `expression` is NULL (DuckDB's
/// `FilterRejectsNull`).
fn filter_rejects_null(filter: &Expression, expression: &Expression) -> bool {
    match (&filter.kind, filter.expression_type) {
        (ExpressionKind::Conjunction(conjunction), ExpressionType::ConjunctionAnd) => conjunction
            .children
            .iter()
            .any(|child| filter_rejects_null(child, expression)),
        (ExpressionKind::Conjunction(conjunction), ExpressionType::ConjunctionOr) => {
            !conjunction.children.is_empty()
                && conjunction
                    .children
                    .iter()
                    .all(|child| filter_rejects_null(child, expression))
        }
        (ExpressionKind::Operator(operator), ExpressionType::OperatorIsNotNull) => operator
            .children
            .first()
            .is_some_and(|child| child.equals(expression)),
        (
            ExpressionKind::Comparison(comparison),
            ExpressionType::CompareEqual
            | ExpressionType::CompareNotequal
            | ExpressionType::CompareLessthan
            | ExpressionType::CompareGreaterthan
            | ExpressionType::CompareLessthanorequalto
            | ExpressionType::CompareGreaterthanorequalto,
        ) => comparison.left.equals(expression) || comparison.right.equals(expression),
        _ => false,
    }
}

/// Whether a join outputs the rows of its child `child_index` unchanged, without
/// NULL-padding them (DuckDB's `JoinOutputPreservesChildNonNullability`).
const fn join_preserves_child_non_nullability(join_type: JoinType, child_index: usize) -> bool {
    match join_type {
        JoinType::Inner => child_index < 2,
        JoinType::Left | JoinType::Semi | JoinType::Anti | JoinType::Mark | JoinType::Single => {
            child_index == 0
        }
        JoinType::Right | JoinType::RightSemi | JoinType::RightAnti => child_index == 1,
        _ => false,
    }
}

/// The statistics of column `column` of the scanned base table, as the storage keeps
/// them (DuckDB's `DuckTableEntry::GetStatistics`): none for the row id and generated
/// columns.
fn table_statistics(get: &rustdb_planner::operator::LogicalGet, column: u64) -> Option<BaseStatistics> {
    if column == COLUMN_IDENTIFIER_ROW_ID {
        return None;
    }
    let entry = get.table.as_ref()?;
    let EntryKind::Table(table) = &entry.kind else {
        return None;
    };
    let storage = table.storage.as_ref()?;
    let physical = table.logical_to_physical(usize::try_from(column).ok()?)?;
    storage.column_statistics(physical)
}

impl<'a> NotNullExpressionAnalyzer<'a> {
    pub const fn new(plan_root: Option<&'a LogicalOperator>) -> Self {
        Self { plan_root }
    }

    /// Whether `expression`, evaluated over the output of `op`, is never NULL (DuckDB's
    /// `NotNullExpressionAnalyzer::IsNotNull`).
    pub fn is_not_null(&self, op: &LogicalOperator, expression: &Expression) -> bool {
        let mut seen_ctes = Vec::new();
        self.is_not_null_inner(op, expression, &mut seen_ctes)
    }

    /// The materialized CTE `cte_index` of the plan (DuckDB's `FindCTE`).
    fn find_cte(&self, cte_index: u64) -> Option<&'a LogicalOperator> {
        let mut operators = vec![self.plan_root?];
        while let Some(op) = operators.pop() {
            if let LogicalKind::MaterializedCte { table_index, .. } = &op.kind
                && *table_index == cte_index
            {
                return Some(op);
            }
            operators.extend(op.children.iter());
        }
        None
    }

    fn is_not_null_inner(
        &self,
        op: &LogicalOperator,
        expression: &Expression,
        seen_ctes: &mut Vec<u64>,
    ) -> bool {
        match &op.kind {
            LogicalKind::Projection { .. } => {
                let [child] = op.children.as_slice() else {
                    return false;
                };
                let Some(binding) = column_ref_binding(expression) else {
                    return false;
                };
                op.column_bindings()
                    .iter()
                    .position(|candidate| *candidate == binding)
                    .and_then(|index| op.expressions.get(index))
                    .is_some_and(|projected| self.is_not_null_inner(child, projected, seen_ctes))
            }
            LogicalKind::Filter { .. } => {
                if op
                    .expressions
                    .iter()
                    .any(|filter| filter_rejects_null(filter, expression))
                {
                    return true;
                }
                matches!(op.children.as_slice(), [child] if self.is_not_null_inner(child, expression, seen_ctes))
            }
            LogicalKind::ComparisonJoin(join)
                if op.operator_type() == rustdb_planner::LogicalOperatorType::LogicalComparisonJoin =>
            {
                if op.children.len() != 2 {
                    return false;
                }
                let Some(binding) = column_ref_binding(expression) else {
                    return false;
                };
                if !op.column_bindings().contains(&binding) {
                    return false;
                }
                let mut binding_child = None;
                for (index, child) in op.children.iter().enumerate() {
                    if !child.column_bindings().contains(&binding) {
                        continue;
                    }
                    if binding_child.is_some() {
                        return false;
                    }
                    binding_child = Some(index);
                }
                let Some(child_index) = binding_child else {
                    return false;
                };
                join_preserves_child_non_nullability(join.join_type, child_index)
                    && self.is_not_null_inner(&op.children[child_index], expression, seen_ctes)
            }
            LogicalKind::Get(get) => {
                let Some(binding) = column_ref_binding(expression) else {
                    return false;
                };
                if binding.table_index != get.table_index {
                    return false;
                }
                let Some(column) = get.column_ids.get(binding.column_index as usize) else {
                    return false;
                };
                if !column.child_indexes.is_empty() {
                    return false;
                }
                // Unknown statistics are no proof.
                table_statistics(get, column.index)
                    .is_some_and(|stats| stats.can_have_no_null() && !stats.can_have_null())
            }
            LogicalKind::CteRef {
                table_index,
                cte_index,
                ..
            } => {
                let Some(binding) = column_ref_binding(expression) else {
                    return false;
                };
                if binding.table_index != *table_index || seen_ctes.contains(cte_index) {
                    return false;
                }
                let Some(source) = self.find_cte(*cte_index).and_then(|cte| cte.children.first()) else {
                    return false;
                };
                let column = binding.column_index as usize;
                let source_bindings = source.column_bindings();
                let (Some(&source_binding), Some(source_type)) =
                    (source_bindings.get(column), source.types.get(column))
                else {
                    return false;
                };
                seen_ctes.push(*cte_index);
                let source_expression =
                    Expression::column_ref(String::new(), source_type.clone(), source_binding, 0);
                let result = self.is_not_null_inner(source, &source_expression, seen_ctes);
                seen_ctes.pop();
                result
            }
            _ => false,
        }
    }
}
