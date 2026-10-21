//! Statistics of expressions (DuckDB's `statistics/expression/propagate_*.cpp`):
//! comparisons, conjunctions, BETWEENs and NULL tests the statistics decide are folded.

use rustdb_ast::ExpressionType;
use rustdb_common::Result;
use rustdb_planner::{Expression, ExpressionKind};
use rustdb_storage::statistics::{BaseStatistics, FilterPropagateResult, StatisticsType};
use rustdb_types::{LogicalTypeId, PhysicalType, Value};

use super::{
    StatisticsPropagator, function, greater, greater_equal, has_min_max, numeric_max, numeric_min,
    numeric_value, set_max, set_min,
};
use crate::expression_util::{boolean_value, constant_or_null, evaluate_scalar};

/// Whether the comparison treats NULLs as values.
pub(super) const fn is_compare_distinct(comparison: ExpressionType) -> bool {
    matches!(
        comparison,
        ExpressionType::CompareDistinctFrom | ExpressionType::CompareNotDistinctFrom
    )
}

impl StatisticsPropagator<'_> {
    /// The statistics of an expression, simplifying it on the way (DuckDB's
    /// `PropagateExpression`).
    pub(super) fn propagate_expression(
        &mut self,
        expression: &mut Expression,
    ) -> Result<Option<BaseStatistics>> {
        match &expression.kind {
            ExpressionKind::Aggregate(_) => self.propagate_aggregate_expression(expression),
            ExpressionKind::Between(_) => self.propagate_between(expression),
            ExpressionKind::Case(_) => self.propagate_case(expression),
            ExpressionKind::Conjunction(_) => self.propagate_conjunction(expression),
            ExpressionKind::Function(_) => self.propagate_function(expression),
            ExpressionKind::Cast(_) => self.propagate_cast(expression),
            ExpressionKind::Comparison(_) => self.propagate_comparison_expression(expression),
            ExpressionKind::Constant(constant) => Ok(Some(BaseStatistics::from_constant(&constant.value))),
            ExpressionKind::ColumnRef(column) => Ok(self.statistics_map.get(&column.binding).cloned()),
            ExpressionKind::Operator(_) => self.propagate_operator(expression),
            _ => {
                let mut result = Ok(());
                expression.for_each_child_mut(|child| {
                    if result.is_ok() {
                        result = self.propagate_expression(child).map(|_| ());
                    }
                });
                result?;
                Ok(None)
            }
        }
    }

    /// Replaces `expression` by a boolean constant and returns its statistics.
    fn replace_with_boolean(expression: &mut Expression, value: bool) -> BaseStatistics {
        let constant = Value::boolean(value);
        let stats = BaseStatistics::from_constant(&constant);
        *expression = Expression::constant(constant);
        stats
    }

    /// Replaces `expression` by `constant_or_null(value, children...)`.
    fn replace_with_constant_or_null(
        &self,
        expression: &mut Expression,
        children: Vec<Expression>,
        value: bool,
    ) -> Result<()> {
        *expression = constant_or_null(self.optimizer.context(), children, Value::boolean(value))?;
        Ok(())
    }

    /// Whether `left <comparison> right` is decided by the statistics (DuckDB's
    /// `PropagateComparison`; numeric types only).
    pub(super) fn propagate_comparison(
        left: &BaseStatistics,
        right: &BaseStatistics,
        comparison: ExpressionType,
    ) -> FilterPropagateResult {
        use FilterPropagateResult as R;
        if !matches!(
            left.logical_type().physical_type(),
            PhysicalType::Bool
                | PhysicalType::UInt8
                | PhysicalType::UInt16
                | PhysicalType::UInt32
                | PhysicalType::UInt64
                | PhysicalType::UInt128
                | PhysicalType::Int8
                | PhysicalType::Int16
                | PhysicalType::Int32
                | PhysicalType::Int64
                | PhysicalType::Int128
                | PhysicalType::Float
                | PhysicalType::Double
        ) {
            return R::NoPruningPossible;
        }
        if !has_min_max(left) || !has_min_max(right) {
            return R::NoPruningPossible;
        }
        let (Some(lmin), Some(lmax), Some(rmin), Some(rmax)) = (
            numeric_min(left),
            numeric_max(left),
            numeric_min(right),
            numeric_max(right),
        ) else {
            return R::NoPruningPossible;
        };
        if lmin.compare(rmin).is_none() {
            // Different physical types: nothing can be said.
            return R::NoPruningPossible;
        }
        let has_null = left.can_have_null() || right.can_have_null();
        let always_true = if has_null {
            R::FilterTrueOrNull
        } else {
            R::FilterAlwaysTrue
        };
        let always_false = if has_null {
            R::FilterFalseOrNull
        } else {
            R::FilterAlwaysFalse
        };
        match comparison {
            ExpressionType::CompareEqual => {
                if greater(lmin, rmax) || greater(rmin, lmax) {
                    always_false
                } else {
                    R::NoPruningPossible
                }
            }
            ExpressionType::CompareGreaterthan => {
                if greater(lmin, rmax) {
                    always_true
                } else if greater_equal(rmin, lmax) {
                    always_false
                } else {
                    R::NoPruningPossible
                }
            }
            ExpressionType::CompareGreaterthanorequalto => {
                if greater_equal(lmin, rmax) {
                    always_true
                } else if greater(rmin, lmax) {
                    always_false
                } else {
                    R::NoPruningPossible
                }
            }
            ExpressionType::CompareLessthan => {
                if greater(rmin, lmax) {
                    always_true
                } else if greater_equal(lmin, rmax) {
                    always_false
                } else {
                    R::NoPruningPossible
                }
            }
            ExpressionType::CompareLessthanorequalto => {
                if greater_equal(rmin, lmax) {
                    always_true
                } else if greater(lmin, rmax) {
                    always_false
                } else {
                    R::NoPruningPossible
                }
            }
            _ => R::NoPruningPossible,
        }
    }

    /// DuckDB's `PropagateExpression(BoundComparisonExpression)`.
    fn propagate_comparison_expression(
        &mut self,
        expression: &mut Expression,
    ) -> Result<Option<BaseStatistics>> {
        let comparison_type = expression.expression_type;
        let ExpressionKind::Comparison(comparison) = &mut expression.kind else {
            return Ok(None);
        };
        let left_stats = self.propagate_expression(&mut comparison.left)?;
        let right_stats = self.propagate_expression(&mut comparison.right)?;
        let (Some(left_stats), Some(right_stats)) = (left_stats, right_stats) else {
            return Ok(None);
        };
        match Self::propagate_comparison(&left_stats, &right_stats, comparison_type) {
            FilterPropagateResult::FilterAlwaysTrue => Ok(Some(Self::replace_with_boolean(expression, true))),
            FilterPropagateResult::FilterAlwaysFalse => {
                Ok(Some(Self::replace_with_boolean(expression, false)))
            }
            result @ (FilterPropagateResult::FilterTrueOrNull | FilterPropagateResult::FilterFalseOrNull) => {
                let placeholder = Expression::constant(Value::boolean(true));
                let ExpressionKind::Comparison(comparison) =
                    std::mem::replace(&mut expression.kind, placeholder.kind)
                else {
                    return Ok(None);
                };
                let comparison = *comparison;
                let value = result == FilterPropagateResult::FilterTrueOrNull;
                self.replace_with_constant_or_null(
                    expression,
                    vec![comparison.left, comparison.right],
                    value,
                )?;
                Ok(None)
            }
            FilterPropagateResult::NoPruningPossible => Ok(None),
        }
    }

    /// DuckDB's `PropagateExpression(BoundConjunctionExpression)`: constant children
    /// are pruned or decide the whole conjunction.
    fn propagate_conjunction(&mut self, expression: &mut Expression) -> Result<Option<BaseStatistics>> {
        let is_and = expression.expression_type == ExpressionType::ConjunctionAnd;
        let ExpressionKind::Conjunction(conjunction) = &mut expression.kind else {
            return Ok(None);
        };
        let mut index = 0;
        let mut replacement = None;
        while index < conjunction.children.len() {
            let child = &mut conjunction.children[index];
            self.propagate_expression(child)?;
            if !child.is_foldable() {
                index += 1;
                continue;
            }
            let constant = evaluate_scalar(self.optimizer.context(), child)?;
            let Some(value) = boolean_value(&constant)? else {
                index += 1;
                continue;
            };
            // TRUE in AND / FALSE in OR is pruned; otherwise it decides the result.
            if value == is_and {
                conjunction.children.remove(index);
                continue;
            }
            replacement = Some(value);
            break;
        }
        if let Some(value) = replacement {
            return Ok(Some(Self::replace_with_boolean(expression, value)));
        }
        match conjunction.children.len() {
            0 => Ok(Some(Self::replace_with_boolean(expression, is_and))),
            1 => {
                if let Some(child) = conjunction.children.pop() {
                    *expression = child;
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// DuckDB's `PropagateExpression(BoundBetweenExpression)`.
    fn propagate_between(&mut self, expression: &mut Expression) -> Result<Option<BaseStatistics>> {
        use FilterPropagateResult as R;
        let ExpressionKind::Between(between) = &mut expression.kind else {
            return Ok(None);
        };
        let input_stats = self.propagate_expression(&mut between.input)?;
        let lower_stats = self.propagate_expression(&mut between.lower)?;
        let upper_stats = self.propagate_expression(&mut between.upper)?;
        let Some(input_stats) = input_stats else {
            return Ok(None);
        };
        let lower_comparison = between.lower_comparison_type();
        let upper_comparison = between.upper_comparison_type();
        let lower_prune = lower_stats.map_or(R::NoPruningPossible, |lower| {
            Self::propagate_comparison(&input_stats, &lower, lower_comparison)
        });
        let upper_prune = upper_stats.map_or(R::NoPruningPossible, |upper| {
            Self::propagate_comparison(&input_stats, &upper, upper_comparison)
        });
        if lower_prune == R::FilterAlwaysTrue && upper_prune == R::FilterAlwaysTrue {
            Self::replace_with_boolean(expression, true);
            return Ok(None);
        }
        if lower_prune == R::FilterAlwaysFalse || upper_prune == R::FilterAlwaysFalse {
            Self::replace_with_boolean(expression, false);
            return Ok(None);
        }
        let take = |expression: &mut Expression| {
            let placeholder = Expression::constant(Value::boolean(true));
            match std::mem::replace(&mut expression.kind, placeholder.kind) {
                ExpressionKind::Between(between) => Some(*between),
                _ => None,
            }
        };
        if lower_prune == R::FilterFalseOrNull || upper_prune == R::FilterFalseOrNull {
            if let Some(between) = take(expression) {
                self.replace_with_constant_or_null(
                    expression,
                    vec![between.input, between.lower, between.upper],
                    false,
                )?;
            }
        } else if lower_prune == R::FilterTrueOrNull && upper_prune == R::FilterTrueOrNull {
            if let Some(between) = take(expression) {
                self.replace_with_constant_or_null(
                    expression,
                    vec![between.input, between.lower, between.upper],
                    true,
                )?;
            }
        } else if lower_prune == R::FilterAlwaysTrue {
            if let Some(between) = take(expression) {
                *expression = Expression::comparison(upper_comparison, between.input, between.upper);
            }
        } else if upper_prune == R::FilterAlwaysTrue
            && let Some(between) = take(expression)
        {
            *expression = Expression::comparison(lower_comparison, between.input, between.lower);
        }
        Ok(None)
    }

    /// DuckDB's `PropagateExpression(BoundCaseExpression)`: the merged statistics of
    /// the results.
    fn propagate_case(&mut self, expression: &mut Expression) -> Result<Option<BaseStatistics>> {
        let ExpressionKind::Case(case) = &mut expression.kind else {
            return Ok(None);
        };
        let mut result = self.propagate_expression(&mut case.else_expr)?;
        for check in &mut case.case_checks {
            self.propagate_expression(&mut check.when_expr)?;
            let then_stats = self.propagate_expression(&mut check.then_expr)?;
            match (then_stats, &mut result) {
                (None, _) => result = None,
                (Some(then_stats), Some(result)) => result.merge(&then_stats),
                (Some(_), None) => {}
            }
        }
        Ok(result)
    }

    /// DuckDB's `PropagateExpression(BoundCastExpression)`.
    fn propagate_cast(&mut self, expression: &mut Expression) -> Result<Option<BaseStatistics>> {
        let target = expression.return_type.clone();
        let ExpressionKind::Cast(cast) = &mut expression.kind else {
            return Ok(None);
        };
        let Some(child_stats) = self.propagate_expression(&mut cast.child)? else {
            return Ok(None);
        };
        let mut result = try_propagate_cast(&child_stats, &cast.child.return_type, &target);
        if cast.try_cast
            && let Some(result) = &mut result
        {
            result.set_has_null();
        }
        Ok(result)
    }

    /// DuckDB's `PropagateExpression(BoundFunctionExpression)`.
    fn propagate_function(&mut self, expression: &mut Expression) -> Result<Option<BaseStatistics>> {
        let ExpressionKind::Function(function) = &mut expression.kind else {
            return Ok(None);
        };
        let mut child_stats = Vec::with_capacity(function.children.len());
        for child in &mut function.children {
            let stats = self.propagate_expression(child)?;
            child_stats
                .push(stats.unwrap_or_else(|| BaseStatistics::create_unknown(child.return_type.clone())));
        }
        function::propagate_scalar_function(self, expression, &child_stats)
    }

    /// DuckDB's `PropagateExpression(BoundAggregateExpression)`.
    fn propagate_aggregate_expression(
        &mut self,
        expression: &mut Expression,
    ) -> Result<Option<BaseStatistics>> {
        let ExpressionKind::Aggregate(aggregate) = &mut expression.kind else {
            return Ok(None);
        };
        let mut child_stats = Vec::with_capacity(aggregate.children.len());
        for child in &mut aggregate.children {
            let stats = self.propagate_expression(child)?;
            child_stats
                .push(stats.unwrap_or_else(|| BaseStatistics::create_unknown(child.return_type.clone())));
        }
        function::propagate_aggregate_function(self, expression, &child_stats)
    }

    /// DuckDB's `PropagateExpression(BoundOperatorExpression)`: COALESCE drops
    /// children that are always or never NULL; NULL tests the statistics decide fold.
    fn propagate_operator(&mut self, expression: &mut Expression) -> Result<Option<BaseStatistics>> {
        let expression_type = expression.expression_type;
        let ExpressionKind::Operator(operator) = &mut expression.kind else {
            return Ok(None);
        };
        let mut child_stats = Vec::with_capacity(operator.children.len());
        let mut all_have_stats = true;
        for child in &mut operator.children {
            let stats = self.propagate_expression(child)?;
            all_have_stats &= stats.is_some();
            child_stats.push(stats);
        }
        if !all_have_stats {
            return Ok(None);
        }
        let mut child_stats: Vec<BaseStatistics> = child_stats.into_iter().flatten().collect();
        match expression_type {
            ExpressionType::OperatorCoalesce => {
                let mut index = 0;
                while index < operator.children.len() {
                    if !child_stats[index].can_have_no_null() {
                        // Always NULL: drop it unless it is the last one.
                        if operator.children.len() > 1 {
                            operator.children.remove(index);
                            child_stats.remove(index);
                            continue;
                        }
                    } else if !child_stats[index].can_have_null() {
                        // Never NULL: the later children do not matter.
                        operator.children.truncate(index + 1);
                        child_stats.truncate(index + 1);
                        break;
                    }
                    index += 1;
                }
                let mut stats = child_stats.into_iter();
                let Some(mut result) = stats.next() else {
                    return Ok(None);
                };
                if operator.children.len() == 1 {
                    if let Some(child) = operator.children.pop() {
                        *expression = child;
                    }
                } else {
                    for other in stats {
                        result.merge(&other);
                    }
                }
                Ok(Some(result))
            }
            ExpressionType::OperatorIsNull | ExpressionType::OperatorIsNotNull => {
                let is_null = expression_type == ExpressionType::OperatorIsNull;
                let Some(child) = child_stats.first() else {
                    return Ok(None);
                };
                if !child.can_have_null() {
                    return Ok(Some(Self::replace_with_boolean(expression, !is_null)));
                }
                if !child.can_have_no_null() {
                    return Ok(Some(Self::replace_with_boolean(expression, is_null)));
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}

/// DuckDB's `StatisticsPropagateVariant`: a cast of a fully shredded VARIANT to a
/// primitive type has the statistics of the typed values, cast if they are of another type.
fn propagate_variant_cast(
    stats: &BaseStatistics,
    target: &rustdb_types::LogicalType,
) -> Option<BaseStatistics> {
    use rustdb_storage::statistics::{is_fully_shredded, structured_type, typed_stats};
    if target.is_nested() || target.id() == LogicalTypeId::Variant {
        return None;
    }
    let shredded = stats.variant()?.shredded_stats()?;
    if !is_fully_shredded(shredded) {
        // Partially shredded: no statistics.
        return None;
    }
    let structured = structured_type(shredded.logical_type());
    let typed = typed_stats(shredded);
    if structured == *target {
        return Some(typed.clone());
    }
    try_propagate_cast(typed, &structured, target)
}

/// Whether statistics survive a cast (DuckDB's `StatisticsPropagator::CanPropagateCast`):
/// only between numeric types, and not between differently scaled timestamps.
pub(super) fn can_propagate_cast(
    source: &rustdb_types::LogicalType,
    target: &rustdb_types::LogicalType,
) -> bool {
    use LogicalTypeId as T;
    if source == target {
        return true;
    }
    let numeric = |ty: &rustdb_types::LogicalType| {
        matches!(
            ty.physical_type(),
            PhysicalType::Int8
                | PhysicalType::Int16
                | PhysicalType::Int32
                | PhysicalType::Int64
                | PhysicalType::Int128
                | PhysicalType::Float
                | PhysicalType::Double
        )
    };
    if !numeric(source) || !numeric(target) {
        return false;
    }
    let source_id = source.id();
    match target.id() {
        T::Time => !matches!(
            source_id,
            T::Timestamp | T::TimestampSec | T::TimestampMs | T::TimestampNs | T::TimestampTz
        ),
        T::Timestamp => !matches!(
            source_id,
            T::TimestampNs | T::TimestampMs | T::TimestampSec | T::TimestampTz
        ),
        T::TimestampTz => !matches!(
            source_id,
            T::TimestampNs | T::TimestampMs | T::TimestampSec | T::Timestamp
        ),
        T::TimestampNs => !matches!(
            source_id,
            T::Timestamp | T::TimestampTz | T::TimestampMs | T::TimestampSec
        ),
        T::TimestampMs => !matches!(
            source_id,
            T::Timestamp | T::TimestampTz | T::TimestampNs | T::TimestampSec
        ),
        T::TimestampSec => !matches!(
            source_id,
            T::Timestamp | T::TimestampTz | T::TimestampNs | T::TimestampMs
        ),
        _ => true,
    }
}

/// The statistics of a cast of values with `stats` (DuckDB's `TryPropagateCast`): of a
/// shredded VARIANT, or of numeric to numeric casts.
pub(super) fn try_propagate_cast(
    stats: &BaseStatistics,
    source: &rustdb_types::LogicalType,
    target: &rustdb_types::LogicalType,
) -> Option<BaseStatistics> {
    use rustdb_function::cast::ValueCast;
    if source.id() == LogicalTypeId::Variant {
        return propagate_variant_cast(stats, target);
    }
    if !can_propagate_cast(source, target) {
        return None;
    }
    if stats.stats_type() != StatisticsType::Numeric || !has_min_max(stats) {
        return None;
    }
    let min = stats.numeric_min()?.try_cast(target).ok()?;
    let max = stats.numeric_max()?.try_cast(target).ok()?;
    let mut result = BaseStatistics::create_empty(target.clone());
    result.set_validity(stats.can_have_null(), stats.can_have_no_null());
    result.set_distinct_count(stats.distinct_count());
    set_min(&mut result, numeric_value(&min));
    set_max(&mut result, numeric_value(&max));
    Some(result)
}
