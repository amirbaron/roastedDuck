//! Filter pushdown through joins and cross products (DuckDB's `pushdown_*_join.cpp` and
//! `pushdown_cross_product.cpp`).

use std::collections::{BTreeMap, BTreeSet};

use roastedduck_ast::{ExpressionType, JoinRefType, JoinType};
use roastedduck_common::{Error, Result};
use roastedduck_planner::operator::JoinCondition;
use roastedduck_planner::{
    Expression, ExpressionKind, JoinSide, LogicalKind, LogicalOperator, LogicalOperatorType, create_join,
    extract_join_conditions,
};
use roastedduck_types::Value;

use super::{Filter, FilterPushdown, empty_result, table_references};
use crate::expression_rewriter::ExpressionRewriter;
use crate::expression_util::{
    boolean_value, can_throw, enumerate_expression, evaluate_scalar, replace_column_refs,
};
use crate::filter_combiner::{FilterCombiner, FilterResult};

/// The side of a join a set of tables belongs to (DuckDB's `JoinSide::GetJoinSide` over
/// bindings): tables not on the left are on the right.
fn join_side(bindings: &BTreeSet<u64>, left: &BTreeSet<u64>) -> JoinSide {
    bindings.iter().fold(JoinSide::None, |side, table| {
        side.combine(if left.contains(table) {
            JoinSide::Left
        } else {
            JoinSide::Right
        })
    })
}

/// The join type and projection maps of a join operator.
fn join_info(op: &LogicalOperator) -> Option<(JoinType, Vec<u64>, Vec<u64>)> {
    match &op.kind {
        LogicalKind::ComparisonJoin(join) => Some((
            join.join_type,
            join.left_projection_map.clone(),
            join.right_projection_map.clone(),
        )),
        LogicalKind::AnyJoin { join_type, .. } => Some((*join_type, Vec::new(), Vec::new())),
        _ => None,
    }
}

fn set_join_type(op: &mut LogicalOperator, join_type: JoinType) {
    match &mut op.kind {
        LogicalKind::ComparisonJoin(join) => join.join_type = join_type,
        LogicalKind::AnyJoin { join_type: ty, .. } => *ty = join_type,
        _ => {}
    }
}

/// Takes the two inputs of a join.
fn take_children(op: &mut LogicalOperator) -> Result<(LogicalOperator, LogicalOperator)> {
    let mut children = std::mem::take(&mut op.children).into_iter();
    match (children.next(), children.next()) {
        (Some(left), Some(right)) => Ok((left, right)),
        _ => Err(Error::internal("join without two inputs")),
    }
}

/// Replaces column references of the right side by NULL (DuckDB's `ReplaceColRefWithNull`).
fn replace_right_with_null(expression: &mut Expression, right: &BTreeSet<u64>) {
    replace_column_refs(expression, &mut |column| {
        let binding = column.as_column_ref()?.binding;
        right
            .contains(&binding.table_index)
            .then(|| Expression::constant(Value::null(column.return_type.clone())))
    });
}

impl FilterPushdown<'_> {
    /// DuckDB's `PushdownJoin`: dispatches on the join type and restores the projection
    /// maps of the join afterwards.
    pub(super) fn pushdown_join(&mut self, op: LogicalOperator) -> Result<LogicalOperator> {
        let Some((join_type, left_projection_map, right_projection_map)) = join_info(&op) else {
            return self.finish_pushdown(op);
        };
        let restore = !left_projection_map.is_empty() || !right_projection_map.is_empty();
        let left_bindings = table_references(&op.children[0]);
        let right_bindings = table_references(&op.children[1]);
        let is_asof = op.operator_type() == LogicalOperatorType::LogicalAsofJoin;
        let mut result = match join_type {
            JoinType::Outer => self.pushdown_outer_join(op, &left_bindings, &right_bindings)?,
            JoinType::Inner if is_asof => self.pushdown_left_join(op, &left_bindings, &right_bindings)?,
            JoinType::Inner => self.pushdown_inner_join(op, &left_bindings, &right_bindings)?,
            JoinType::Left => self.pushdown_left_join(op, &left_bindings, &right_bindings)?,
            JoinType::Mark => self.pushdown_mark_join(op, &left_bindings, right_bindings)?,
            JoinType::Single => self.pushdown_single_join(op, &left_bindings)?,
            JoinType::Semi | JoinType::Anti => self.pushdown_semi_anti_join(op)?,
            _ => return self.finish_pushdown(op),
        };
        if restore && let LogicalKind::ComparisonJoin(join) = &mut result.kind {
            join.left_projection_map = left_projection_map;
            join.right_projection_map = right_projection_map;
        }
        Ok(result)
    }

    /// DuckDB's `PushdownInnerJoin`: the conditions become filters and the join a cross
    /// product, which the cross product pushdown turns back into a join.
    fn pushdown_inner_join(
        &mut self,
        mut op: LogicalOperator,
        _left: &BTreeSet<u64>,
        _right: &BTreeSet<u64>,
    ) -> Result<LogicalOperator> {
        if op.operator_type() == LogicalOperatorType::LogicalDelimJoin {
            let op = self.push_filters_into_delim_join(op)?;
            return self.finish_pushdown(op);
        }
        match &mut op.kind {
            LogicalKind::AnyJoin { condition, .. } => {
                let condition =
                    std::mem::replace(&mut **condition, Expression::constant(Value::boolean(true)));
                if self.add_filter(condition)? == FilterResult::Unsatisfiable {
                    return empty_result(op);
                }
            }
            LogicalKind::ComparisonJoin(join) => {
                for condition in std::mem::take(&mut join.conditions) {
                    if self.add_filter(condition.into_expression())? == FilterResult::Unsatisfiable {
                        return empty_result(op);
                    }
                }
            }
            _ => {}
        }
        self.generate_filters();
        let estimated_cardinality = op.estimated_cardinality;
        let (left, right) = take_children(&mut op)?;
        let mut cross_product =
            LogicalOperator::new(LogicalKind::CrossProduct).with_children(vec![left, right]);
        cross_product.estimated_cardinality = estimated_cardinality;
        self.pushdown_cross_product(cross_product)
    }

    /// DuckDB's `PushdownCrossProduct`: filters on one side go into it, filters over
    /// both sides become the conditions of a join.
    pub(super) fn pushdown_cross_product(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        let mut left_pushdown = self.child();
        let mut right_pushdown = self.child();
        let mut join_expressions = Vec::new();
        let left_bindings = table_references(&op.children[0]);
        if !self.filters.is_empty() {
            for filter in std::mem::take(&mut self.filters) {
                match join_side(&filter.bindings, &left_bindings) {
                    JoinSide::Left => left_pushdown.filters.push(filter),
                    JoinSide::Right => right_pushdown.filters.push(filter),
                    JoinSide::Both | JoinSide::None => join_expressions.push(filter.filter),
                }
            }
        }
        let (left, right) = take_children(&mut op)?;
        let mut left = left_pushdown.rewrite(left)?;
        let mut right = right_pushdown.rewrite(right)?;
        if join_expressions.is_empty() {
            op.children = vec![left, right];
            return Ok(op);
        }
        let mut conditions: Vec<JoinCondition> = Vec::new();
        let mut arbitrary_expressions = Vec::new();
        extract_join_conditions(
            self.optimizer.context(),
            JoinType::Inner,
            JoinRefType::Regular,
            &mut left,
            &mut right,
            join_expressions,
            &mut conditions,
            &mut arbitrary_expressions,
        )?;
        let mut new_op = create_join(
            JoinType::Inner,
            JoinRefType::Regular,
            left,
            right,
            conditions,
            arbitrary_expressions,
        )?;
        if let Some(estimate) = op.estimated_cardinality {
            new_op.estimated_cardinality = Some(estimate);
            if matches!(new_op.kind, LogicalKind::Filter { .. })
                && let Some(join) = new_op.children.first_mut()
            {
                join.estimated_cardinality = Some(estimate);
            }
        }
        Ok(new_op)
    }

    /// Whether a filter is NULL or false once the right side is NULL, i.e. it removes the
    /// rows a LEFT join adds (DuckDB's `FilterRemovesNull`).
    fn filter_removes_null(&self, expression: &Expression, right: &BTreeSet<u64>) -> Result<bool> {
        let mut copy = expression.clone();
        replace_right_with_null(&mut copy, right);
        let mut filter = LogicalOperator::new(LogicalKind::Filter {
            projection_map: Vec::new(),
        })
        .with_expressions(vec![copy]);
        ExpressionRewriter::with_context(self.optimizer.context()).visit_operator(&mut filter)?;
        for expression in &filter.expressions {
            if !expression.is_foldable() {
                return Ok(false);
            }
            let value = evaluate_scalar(self.optimizer.context(), expression)?;
            if boolean_value(&value)? != Some(true) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// DuckDB's `PushdownLeftJoin`.
    fn pushdown_left_join(
        &mut self,
        mut op: LogicalOperator,
        left_bindings: &BTreeSet<u64>,
        right_bindings: &BTreeSet<u64>,
    ) -> Result<LogicalOperator> {
        let mut left_pushdown = self.child();
        let mut right_pushdown = self.child();
        let mut filter_combiner = FilterCombiner::new(self.optimizer.context());
        let op_type = op.operator_type();
        let is_comparison = matches!(
            op_type,
            LogicalOperatorType::LogicalComparisonJoin | LogicalOperatorType::LogicalAsofJoin
        );
        if is_comparison && let LogicalKind::ComparisonJoin(join) = &op.kind {
            for condition in &join.conditions {
                filter_combiner.add_filter(Expression::comparison(
                    condition.comparison,
                    condition.left.clone(),
                    condition.right.clone(),
                ))?;
            }
        }
        let mut remaining_filters: Vec<Filter> = Vec::new();
        let mut index = 0;
        while index < self.filters.len() {
            let side = join_side(&self.filters[index].bindings, left_bindings);
            if side == JoinSide::Left {
                if is_comparison {
                    filter_combiner.add_filter(self.filters[index].filter.clone())?;
                }
                left_pushdown.filters.push(self.filters.remove(index));
                continue;
            }
            if op_type != LogicalOperatorType::LogicalAsofJoin {
                if self.filter_removes_null(&self.filters[index].filter, right_bindings)? {
                    // The filter removes the rows the LEFT join adds: it is an inner join.
                    set_join_type(&mut op, JoinType::Inner);
                    self.filters.append(&mut left_pushdown.filters);
                    self.filters.append(&mut remaining_filters);
                    return self.pushdown_inner_join(op, left_bindings, right_bindings);
                }
                remaining_filters.push(self.filters.remove(index));
                continue;
            }
            index += 1;
        }
        // Comparisons with a join key on the left also hold for the right key.
        let mut derived = Vec::new();
        filter_combiner.generate_filters(|filter| {
            let tables: BTreeSet<u64> = crate::expression_util::table_indexes(&filter)
                .into_iter()
                .collect();
            if join_side(&tables, left_bindings) == JoinSide::Right {
                derived.push(filter);
            }
        });
        for filter in derived {
            right_pushdown.add_filter(filter)?;
        }
        right_pushdown.generate_filters();
        let (left, right) = take_children(&mut op)?;
        let left = left_pushdown.rewrite(left)?;
        let mut never_matches = false;
        if let LogicalKind::AnyJoin { join_type, condition } = &op.kind {
            let join_type = *join_type;
            let condition = (**condition).clone();
            never_matches =
                self.add_filter(condition)? == FilterResult::Unsatisfiable && join_type == JoinType::Left;
        }
        let op = if never_matches {
            // The condition is never true: the right side contributes one NULL row.
            let null_side = self.null_row_of(right)?;
            LogicalOperator::cross_product(left, null_side)
        } else {
            op.children = vec![left, right_pushdown.rewrite(right)?];
            op
        };
        self.filters.append(&mut remaining_filters);
        Ok(self.push_final_filters(op))
    }

    /// One row of NULLs with the columns of `op`, as cross products of projections over
    /// dummy scans, one per table index (the unsatisfiable-condition case of DuckDB's
    /// `PushdownLeftJoin`).
    fn null_row_of(&self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        let bindings = op.column_bindings();
        op.resolve_operator_types()?;
        let mut groups: BTreeMap<u64, Vec<Expression>> = BTreeMap::new();
        for (binding, ty) in bindings.iter().zip(&op.types) {
            groups
                .entry(binding.table_index)
                .or_default()
                .push(Expression::constant(Value::null(ty.clone())));
        }
        let mut result: Option<LogicalOperator> = None;
        for (table_index, expressions) in groups {
            let dummy = LogicalOperator::new(LogicalKind::DummyScan {
                table_index: self.optimizer.generate_table_index(),
            });
            let projection = LogicalOperator::projection(table_index, expressions, dummy);
            result = Some(match result {
                None => projection,
                Some(left) => LogicalOperator::cross_product(left, projection),
            });
        }
        result.ok_or_else(|| Error::internal("join input without columns"))
    }

    /// DuckDB's `PushdownMarkJoin`: a filter on the marker alone makes it a SEMI join, a
    /// filter on `NOT marker` with NULL-safe conditions an ANTI join.
    fn pushdown_mark_join(
        &mut self,
        mut op: LogicalOperator,
        left_bindings: &BTreeSet<u64>,
        mut right_bindings: BTreeSet<u64>,
    ) -> Result<LogicalOperator> {
        let LogicalKind::ComparisonJoin(join) = &mut op.kind else {
            return self.finish_pushdown(op);
        };
        right_bindings.insert(join.mark_index);
        let mut left_pushdown = self.child();
        let right_pushdown = self.child();
        let mut index = 0;
        while index < self.filters.len() {
            let side = join_side(&self.filters[index].bindings, left_bindings);
            if side == JoinSide::Left {
                left_pushdown.filters.push(self.filters.remove(index));
                continue;
            }
            if side == JoinSide::Right {
                let filter = &self.filters[index].filter;
                if filter.expression_type == ExpressionType::BoundColumnRef
                    && self.convert_mark_joins
                    && join.convert_mark_to_semi
                {
                    join.join_type = JoinType::Semi;
                    self.filters.remove(index);
                    continue;
                }
                if filter.expression_type == ExpressionType::OperatorNot
                    && let ExpressionKind::Operator(operator) = &filter.kind
                    && operator
                        .children
                        .first()
                        .is_some_and(|child| child.expression_type == ExpressionType::BoundColumnRef)
                {
                    let all_null_values_are_equal = join.conditions.iter().all(|condition| {
                        matches!(
                            condition.comparison,
                            ExpressionType::CompareDistinctFrom | ExpressionType::CompareNotDistinctFrom
                        )
                    });
                    if all_null_values_are_equal && self.convert_mark_joins && join.convert_mark_to_semi {
                        join.join_type = JoinType::Anti;
                        self.filters.remove(index);
                        continue;
                    }
                }
            }
            index += 1;
        }
        let (left, right) = take_children(&mut op)?;
        let left = left_pushdown.rewrite(left)?;
        let mut right_pushdown = right_pushdown;
        let right = right_pushdown.rewrite(right)?;
        op.children = vec![left, right];
        Ok(self.push_final_filters(op))
    }

    /// DuckDB's `PushdownSingleJoin`: only filters on the left side move down.
    fn pushdown_single_join(
        &mut self,
        mut op: LogicalOperator,
        left_bindings: &BTreeSet<u64>,
    ) -> Result<LogicalOperator> {
        let mut left_pushdown = self.child();
        let mut right_pushdown = self.child();
        let mut index = 0;
        while index < self.filters.len() {
            if join_side(&self.filters[index].bindings, left_bindings) == JoinSide::Left {
                left_pushdown.filters.push(self.filters.remove(index));
            } else {
                index += 1;
            }
        }
        let (left, right) = take_children(&mut op)?;
        op.children = vec![left_pushdown.rewrite(left)?, right_pushdown.rewrite(right)?];
        Ok(self.push_final_filters(op))
    }

    /// DuckDB's `PushdownSemiAntiJoin`: the filters go to the left side; empty inputs
    /// simplify the join.
    fn pushdown_semi_anti_join(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        let join_type = op.join_type();
        let (left, right) = take_children(&mut op)?;
        let left = self.rewrite(left)?;
        let right = self.child().rewrite(right)?;
        let left_empty = matches!(left.kind, LogicalKind::EmptyResult { .. });
        let right_empty = matches!(right.kind, LogicalKind::EmptyResult { .. });
        op.children = vec![left, right];
        if left_empty && right_empty {
            return empty_result(op);
        }
        if left_empty && matches!(join_type, Some(JoinType::Anti | JoinType::Semi)) {
            return empty_result(op);
        }
        if right_empty {
            match join_type {
                Some(JoinType::Anti) => {
                    let left = op.children.swap_remove(0);
                    return Ok(left);
                }
                Some(JoinType::Semi) => return empty_result(op),
                _ => {}
            }
        }
        Ok(op)
    }

    /// DuckDB's `PushdownOuterJoin`: filters on `COALESCE` of equal join keys go into
    /// both sides.
    fn pushdown_outer_join(
        &mut self,
        mut op: LogicalOperator,
        _left: &BTreeSet<u64>,
        _right: &BTreeSet<u64>,
    ) -> Result<LogicalOperator> {
        if op.operator_type() != LogicalOperatorType::LogicalComparisonJoin {
            return self.finish_pushdown(op);
        }
        let LogicalKind::ComparisonJoin(join) = &op.kind else {
            return self.finish_pushdown(op);
        };
        let mut left_pushdown = self.child();
        let mut right_pushdown = self.child();
        let applied = push_down_filters_on_coalesced_join_keys(
            &mut self.filters,
            &join.conditions,
            &mut left_pushdown,
            &mut right_pushdown,
        )?;
        if !applied {
            return self.finish_pushdown(op);
        }
        left_pushdown.generate_filters();
        right_pushdown.generate_filters();
        let (left, right) = take_children(&mut op)?;
        op.children = vec![left_pushdown.rewrite(left)?, right_pushdown.rewrite(right)?];
        Ok(self.push_final_filters(op))
    }
}

/// The arguments of a COALESCE with nested COALESCEs flattened (DuckDB's
/// `FlattenedCoalesce::Of`).
fn flattened_coalesce<'e>(expressions: &[&'e Expression]) -> Vec<&'e Expression> {
    fn flatten<'e>(expression: &'e Expression, args: &mut Vec<&'e Expression>) {
        if expression.expression_type == ExpressionType::OperatorCoalesce {
            for child in expression.children() {
                flatten(child, args);
            }
        } else {
            args.push(expression);
        }
    }
    let mut args = Vec::new();
    for expression in expressions {
        flatten(expression, &mut args);
    }
    args
}

fn same_arguments(left: &[&Expression], right: &[&Expression]) -> bool {
    left.len() == right.len() && left.iter().zip(right).all(|(l, r)| l.equals(r))
}

/// Replaces every sub-expression equal to one of `targets` by `replacement` (DuckDB's
/// `ReplaceIn`).
fn replace_in(expression: &mut Expression, targets: &[Expression], replacement: &Expression) {
    if targets.iter().any(|target| target.equals(expression)) {
        *expression = replacement.clone();
        return;
    }
    expression.for_each_child_mut(|child| replace_in(child, targets, replacement));
}

/// Replaces every sub-expression equal to one of `args` by a NULL constant.
fn substitute_null(expression: &mut Expression, args: &[Expression]) {
    if args.iter().any(|arg| arg.equals(expression)) {
        *expression = Expression::constant(Value::null(expression.return_type.clone()));
        return;
    }
    expression.for_each_child_mut(|child| substitute_null(child, args));
}

/// Whether the expression is scalar once `args` are replaced by constants (DuckDB's
/// `ExprIsFunctionOnlyOf`).
fn is_function_only_of(expression: &Expression, args: &[Expression]) -> bool {
    let mut copy = expression.clone();
    substitute_null(&mut copy, args);
    copy.is_scalar()
}

/// DuckDB's `PushDownFiltersOnCoalescedEqualJoinKeys`.
fn push_down_filters_on_coalesced_join_keys(
    filters: &mut Vec<Filter>,
    conditions: &[JoinCondition],
    left_pushdown: &mut FilterPushdown<'_>,
    right_pushdown: &mut FilterPushdown<'_>,
) -> Result<bool> {
    let keys: Vec<(Vec<&Expression>, usize)> = conditions
        .iter()
        .enumerate()
        .filter(|(_, condition)| condition.comparison == ExpressionType::CompareEqual)
        .flat_map(|(index, condition)| {
            [
                (flattened_coalesce(&[&condition.left, &condition.right]), index),
                (flattened_coalesce(&[&condition.right, &condition.left]), index),
            ]
        })
        .collect();
    if keys.is_empty() {
        return Ok(false);
    }
    let mut applied = false;
    let mut index = 0;
    while index < filters.len() {
        let filter = &filters[index].filter;
        if filter.is_volatile() || can_throw(filter) {
            index += 1;
            continue;
        }
        let mut to_replace: Vec<Expression> = Vec::new();
        let mut condition_index: Option<usize> = None;
        let mut many = false;
        enumerate_expression(filter, &mut |sub| {
            if many || sub.expression_type != ExpressionType::OperatorCoalesce {
                return;
            }
            let args = flattened_coalesce(&[sub]);
            let Some((_, found)) = keys.iter().find(|(key, _)| same_arguments(key, &args)) else {
                return;
            };
            if condition_index.is_some_and(|existing| existing != *found) {
                many = true;
                return;
            }
            condition_index = Some(*found);
            if !to_replace.iter().any(|existing| existing.equals(sub)) {
                to_replace.push(sub.clone());
            }
        });
        let Some(condition_index) = condition_index else {
            index += 1;
            continue;
        };
        if to_replace.is_empty() || many || !is_function_only_of(filter, &to_replace) {
            index += 1;
            continue;
        }
        let condition = &conditions[condition_index];
        let mut left_filter = filter.clone();
        replace_in(&mut left_filter, &to_replace, &condition.left);
        let mut right_filter = filter.clone();
        replace_in(&mut right_filter, &to_replace, &condition.right);
        left_pushdown.add_filter(left_filter)?;
        right_pushdown.add_filter(right_filter)?;
        filters.remove(index);
        applied = true;
    }
    Ok(applied)
}
