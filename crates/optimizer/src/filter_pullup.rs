//! Filter pullup (DuckDB's `FilterPullup`): filters below inner joins, cross products
//! and set operations move up so that filter pushdown can then push them into every
//! input they constrain (e.g. through an equality join condition to the other side).

use rustdb_ast::JoinType;
use rustdb_common::Result;
use rustdb_planner::{
    DistinctType, Expression, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType,
};

/// Pulls filters up one subtree (DuckDB's `FilterPullup`).
#[derive(Debug, Default)]
pub struct FilterPullup {
    /// Whether filters may be pulled out of this subtree.
    can_pullup: bool,
    /// Whether projections may gain columns for pulled-up filters (not below INTERSECT,
    /// EXCEPT and DISTINCT, where extra columns change the result).
    can_add_column: bool,
    filters_expr_pullup: Vec<Expression>,
}

fn take_child(op: &mut LogicalOperator, index: usize) -> LogicalOperator {
    std::mem::replace(
        &mut op.children[index],
        LogicalOperator::new(LogicalKind::DummyScan { table_index: 0 }),
    )
}

/// A filter with `expressions` on top of `child` (DuckDB's `GeneratePullupFilter`).
fn generate_pullup_filter(child: LogicalOperator, expressions: Vec<Expression>) -> LogicalOperator {
    LogicalOperator::new(LogicalKind::Filter {
        projection_map: Vec::new(),
    })
    .with_expressions(expressions)
    .with_children(vec![child])
}

/// Makes `expression` reference the projection's outputs, adding projected columns for
/// references it does not output yet (DuckDB's `ReplaceExpressionBinding`).
fn replace_expression_binding(
    projection: &mut Vec<Expression>,
    expression: &mut Expression,
    table_index: u64,
) {
    if let ExpressionKind::ColumnRef(_) = &expression.kind {
        let position = projection.iter().position(|projected| {
            matches!(projected.kind, ExpressionKind::ColumnRef(_)) && projected.equals(expression)
        });
        let column_index = position.unwrap_or_else(|| {
            projection.push(expression.clone());
            projection.len() - 1
        });
        if let ExpressionKind::ColumnRef(column) = &mut expression.kind {
            column.binding.table_index = table_index;
            column.binding.column_index = column_index as u64;
        }
    }
    expression.for_each_child_mut(|child| replace_expression_binding(projection, child, table_index));
}

/// Points the column references of a filter at the set operation's output (DuckDB's
/// `ReplaceFilterTableIndex`).
fn replace_filter_table_index(expression: &mut Expression, table_index: u64) {
    if let ExpressionKind::ColumnRef(column) = &mut expression.kind {
        column.binding.table_index = table_index;
        return;
    }
    expression.for_each_child_mut(|child| replace_filter_table_index(child, table_index));
}

impl FilterPullup {
    pub fn new(can_pullup: bool, can_add_column: bool) -> Self {
        Self {
            can_pullup,
            can_add_column,
            filters_expr_pullup: Vec::new(),
        }
    }

    /// Pulls the filters of `op` up (DuckDB's `FilterPullup::Rewrite`).
    pub fn rewrite(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        match op.operator_type() {
            LogicalOperatorType::LogicalFilter => self.pullup_filter(op),
            LogicalOperatorType::LogicalProjection => self.pullup_projection(op),
            LogicalOperatorType::LogicalCrossProduct => self.pullup_both_side(op),
            LogicalOperatorType::LogicalComparisonJoin
            | LogicalOperatorType::LogicalAnyJoin
            | LogicalOperatorType::LogicalDelimJoin
            | LogicalOperatorType::LogicalAsofJoin => self.pullup_join(op),
            LogicalOperatorType::LogicalIntersect | LogicalOperatorType::LogicalExcept => {
                self.pullup_set_operation(op)
            }
            LogicalOperatorType::LogicalDistinct => {
                if matches!(
                    op.kind,
                    LogicalKind::Distinct {
                        distinct_type: DistinctType::Distinct,
                        ..
                    }
                ) {
                    let child = take_child(&mut op, 0);
                    op.children[0] = self.rewrite(child)?;
                    return Ok(op);
                }
                self.finish_pullup(op)
            }
            LogicalOperatorType::LogicalOrderBy => {
                let child = take_child(&mut op, 0);
                op.children[0] = self.rewrite(child)?;
                Ok(op)
            }
            _ => self.finish_pullup(op),
        }
    }

    fn pullup_join(&mut self, op: LogicalOperator) -> Result<LogicalOperator> {
        match op.join_type() {
            Some(JoinType::Inner) if op.operator_type() == LogicalOperatorType::LogicalAsofJoin => {
                self.pullup_from_left(op)
            }
            Some(JoinType::Inner) => self.pullup_inner_join(op),
            Some(JoinType::Left | JoinType::Anti | JoinType::Semi) => self.pullup_from_left(op),
            _ => self.finish_pullup(op),
        }
    }

    /// DuckDB's `PullupInnerJoin`: the join becomes a cross product with its
    /// conditions as filters.
    fn pullup_inner_join(&mut self, op: LogicalOperator) -> Result<LogicalOperator> {
        if op.operator_type() == LogicalOperatorType::LogicalDelimJoin {
            return Ok(op);
        }
        let mut op = self.pullup_both_side(op)?;
        let mut expressions = Vec::new();
        if matches!(op.kind, LogicalKind::Filter { .. }) {
            expressions = std::mem::take(&mut op.expressions);
            op = take_child(&mut op, 0);
        } else if !self.can_pullup {
            return Ok(op);
        }
        match &mut op.kind {
            LogicalKind::ComparisonJoin(join) => {
                expressions.extend(
                    std::mem::take(&mut join.conditions)
                        .into_iter()
                        .map(rustdb_planner::operator::JoinCondition::into_expression),
                );
            }
            LogicalKind::AnyJoin { condition, .. } => {
                expressions.push(std::mem::replace(
                    &mut **condition,
                    Expression::constant(rustdb_types::Value::boolean(true)),
                ));
            }
            _ => {
                return Err(rustdb_common::Error::not_implemented(format!(
                    "PullupInnerJoin for LogicalOperatorType::{}",
                    op.operator_type().name()
                )));
            }
        }
        let left = take_child(&mut op, 0);
        let right = take_child(&mut op, 1);
        let cross_product = LogicalOperator::new(LogicalKind::CrossProduct).with_children(vec![left, right]);
        if self.can_pullup {
            self.filters_expr_pullup.extend(expressions);
            Ok(cross_product)
        } else {
            Ok(generate_pullup_filter(cross_product, expressions))
        }
    }

    /// Pulls the filters of both inputs up into one filter on top (DuckDB's
    /// `PullupBothSide`).
    fn pullup_both_side(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        let mut left_pullup = Self::new(true, self.can_add_column);
        let mut right_pullup = Self::new(true, self.can_add_column);
        let left = take_child(&mut op, 0);
        op.children[0] = left_pullup.rewrite(left)?;
        let right = take_child(&mut op, 1);
        op.children[1] = right_pullup.rewrite(right)?;
        left_pullup
            .filters_expr_pullup
            .append(&mut right_pullup.filters_expr_pullup);
        if !left_pullup.filters_expr_pullup.is_empty() {
            return Ok(generate_pullup_filter(op, left_pullup.filters_expr_pullup));
        }
        Ok(op)
    }

    /// Pulls up the left input's filters only (DuckDB's `PullupFromLeft`).
    fn pullup_from_left(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        let mut left_pullup = Self::new(true, self.can_add_column);
        let mut right_pullup = Self::new(false, self.can_add_column);
        let left = take_child(&mut op, 0);
        op.children[0] = left_pullup.rewrite(left)?;
        let right = take_child(&mut op, 1);
        op.children[1] = right_pullup.rewrite(right)?;
        if !left_pullup.filters_expr_pullup.is_empty() && right_pullup.filters_expr_pullup.is_empty() {
            return Ok(generate_pullup_filter(op, left_pullup.filters_expr_pullup));
        }
        Ok(op)
    }

    /// DuckDB's `PullupFilter`: the filter's expressions move up, the filter goes away.
    fn pullup_filter(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        let has_projection_map =
            matches!(&op.kind, LogicalKind::Filter { projection_map } if !projection_map.is_empty());
        if self.can_pullup && !has_projection_map {
            let child = take_child(&mut op, 0);
            let child = self.rewrite(child)?;
            self.filters_expr_pullup.append(&mut op.expressions);
            return Ok(child);
        }
        let child = take_child(&mut op, 0);
        op.children[0] = self.rewrite(child)?;
        Ok(op)
    }

    /// DuckDB's `PullupProjection`: the pulled-up filters are rewritten to reference the
    /// projection's outputs.
    fn pullup_projection(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        let child = take_child(&mut op, 0);
        op.children[0] = self.rewrite(child)?;
        if self.filters_expr_pullup.is_empty() {
            return Ok(op);
        }
        let LogicalKind::Projection { table_index } = op.kind else {
            return Ok(op);
        };
        if !self.can_add_column {
            self.project_set_operation(&mut op, table_index);
            return Ok(op);
        }
        for expression in &mut self.filters_expr_pullup {
            replace_expression_binding(&mut op.expressions, expression, table_index);
        }
        Ok(op)
    }

    /// Below INTERSECT, EXCEPT and DISTINCT a projection cannot gain columns: the filters
    /// are pulled through only if the projection outputs everything they reference
    /// (DuckDB's `ProjectSetOperation`).
    fn project_set_operation(&mut self, op: &mut LogicalOperator, table_index: u64) {
        let mut projection = op.expressions.clone();
        let changed: Vec<Expression> = self
            .filters_expr_pullup
            .iter()
            .map(|filter| {
                let mut copy = filter.clone();
                replace_expression_binding(&mut projection, &mut copy, table_index);
                copy
            })
            .collect();
        if projection.len() > op.expressions.len() {
            // Revert: the filters stay below the projection.
            let child = take_child(op, 0);
            op.children[0] = generate_pullup_filter(child, std::mem::take(&mut self.filters_expr_pullup));
            return;
        }
        self.filters_expr_pullup = changed;
    }

    /// DuckDB's `PullupSetOperation`.
    fn pullup_set_operation(&mut self, op: LogicalOperator) -> Result<LogicalOperator> {
        self.can_add_column = false;
        self.can_pullup = true;
        let mut op = if op.operator_type() == LogicalOperatorType::LogicalIntersect {
            self.pullup_both_side(op)?
        } else {
            self.pullup_from_left(op)?
        };
        if matches!(op.kind, LogicalKind::Filter { .. })
            && let Some(LogicalKind::SetOperation { table_index, .. }) =
                op.children.first().map(|child| &child.kind)
        {
            let table_index = *table_index;
            for expression in &mut op.expressions {
                replace_filter_table_index(expression, table_index);
            }
        }
        Ok(op)
    }

    /// Pulls up within the children and puts this level's filters on top (DuckDB's
    /// `FinishPullup`).
    fn finish_pullup(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        for index in 0..op.children.len() {
            let child = take_child(&mut op, index);
            op.children[index] = Self::default().rewrite(child)?;
        }
        if self.filters_expr_pullup.is_empty() {
            return Ok(op);
        }
        Ok(generate_pullup_filter(
            op,
            std::mem::take(&mut self.filters_expr_pullup),
        ))
    }
}
