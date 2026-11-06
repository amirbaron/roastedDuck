//! The filter combiner (DuckDB's `FilterCombiner`): collects AND-ed filters and gives
//! back a smaller, equivalent set. It prunes implied comparisons (`x > 5 AND x > 7`),
//! derives comparisons through equalities (`x = y AND x = 500` gives `y = 500`),
//! detects unsatisfiable combinations and turns comparisons on columns into table scan
//! filters.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use rustdb_ast::ExpressionType;
use rustdb_common::Result;
use rustdb_planner::expression::{BoundBetweenExpression, BoundComparisonExpression};
use std::sync::Arc;

use rustdb_planner::operator::{ColumnIndex, ExpressionTableFilter, LogicalGet};
use rustdb_planner::{ClientContext, Expression, ExpressionKind};
use rustdb_storage::{TableFilter, TableFilterSet};
use rustdb_types::compare::CompareOp;
use rustdb_types::{LogicalType, LogicalTypeId, PhysicalType, Value};

use crate::expression_util::{boolean_value, compare_values, flip_comparison, try_evaluate_scalar};

/// What adding a filter to the combiner found (DuckDB's `FilterResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterResult {
    /// The filters can never all be true.
    Unsatisfiable,
    Success,
    /// The combiner cannot reason about the filter; it is kept as it is.
    Unsupported,
}

/// How much of a filter became table scan filters (DuckDB's `FilterPushdownResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterPushdownResult {
    NoPushdown,
    PushedDownPartially,
    PushedDownFully,
}

/// A comparison of an expression with a constant (DuckDB's `ExpressionValueInformation`).
#[derive(Clone, Debug)]
struct ValueInformation {
    constant: Value,
    comparison_type: ExpressionType,
}

/// Which of two comparisons on the same expression implies the other (DuckDB's
/// `ValueComparisonResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ValueComparisonResult {
    PruneLeft,
    PruneRight,
    UnsatisfiableCondition,
    PruneNothing,
}

const fn is_greater_than(ty: ExpressionType) -> bool {
    matches!(
        ty,
        ExpressionType::CompareGreaterthan | ExpressionType::CompareGreaterthanorequalto
    )
}

const fn is_less_than(ty: ExpressionType) -> bool {
    matches!(
        ty,
        ExpressionType::CompareLessthan | ExpressionType::CompareLessthanorequalto
    )
}

/// The comparisons a combiner reasons about (DuckDB's `SupportedFilterComparison`).
const fn supported_filter_comparison(ty: ExpressionType) -> bool {
    matches!(
        ty,
        ExpressionType::CompareEqual
            | ExpressionType::CompareGreaterthan
            | ExpressionType::CompareGreaterthanorequalto
            | ExpressionType::CompareLessthan
            | ExpressionType::CompareLessthanorequalto
            | ExpressionType::CompareNotequal
    )
}

/// The storage comparison of a comparison expression type.
pub fn compare_op(ty: ExpressionType) -> Option<CompareOp> {
    Some(match ty {
        ExpressionType::CompareEqual => CompareOp::Equal,
        ExpressionType::CompareNotequal => CompareOp::NotEqual,
        ExpressionType::CompareLessthan => CompareOp::LessThan,
        ExpressionType::CompareLessthanorequalto => CompareOp::LessThanOrEqual,
        ExpressionType::CompareGreaterthan => CompareOp::GreaterThan,
        ExpressionType::CompareGreaterthanorequalto => CompareOp::GreaterThanOrEqual,
        _ => return None,
    })
}

fn invert(result: ValueComparisonResult) -> ValueComparisonResult {
    match result {
        ValueComparisonResult::PruneLeft => ValueComparisonResult::PruneRight,
        ValueComparisonResult::PruneRight => ValueComparisonResult::PruneLeft,
        other => other,
    }
}

/// DuckDB's `CompareValueInformation`.
fn compare_value_information(left: &ValueInformation, right: &ValueInformation) -> ValueComparisonResult {
    use ExpressionType as E;
    use ValueComparisonResult as R;
    let order = compare_values(&left.constant, &right.constant).unwrap_or(Ordering::Equal);
    if left.comparison_type == E::CompareEqual {
        let prune_right = match right.comparison_type {
            E::CompareLessthan => order == Ordering::Less,
            E::CompareLessthanorequalto => order != Ordering::Greater,
            E::CompareGreaterthan => order == Ordering::Greater,
            E::CompareGreaterthanorequalto => order != Ordering::Less,
            E::CompareNotequal => order != Ordering::Equal,
            _ => order == Ordering::Equal,
        };
        return if prune_right {
            R::PruneRight
        } else {
            R::UnsatisfiableCondition
        };
    }
    if right.comparison_type == E::CompareEqual {
        return invert(compare_value_information(right, left));
    }
    if left.comparison_type == E::CompareNotequal {
        let prune_left = match right.comparison_type {
            E::CompareLessthan => order != Ordering::Less,
            E::CompareLessthanorequalto => order == Ordering::Greater,
            E::CompareGreaterthan => order != Ordering::Greater,
            E::CompareGreaterthanorequalto => order == Ordering::Less,
            _ => order == Ordering::Equal,
        };
        return if prune_left { R::PruneLeft } else { R::PruneNothing };
    }
    if right.comparison_type == E::CompareNotequal {
        return invert(compare_value_information(right, left));
    }
    if is_greater_than(left.comparison_type) && is_greater_than(right.comparison_type) {
        return match order {
            Ordering::Less => R::PruneLeft,
            Ordering::Equal if left.comparison_type == E::CompareGreaterthanorequalto => R::PruneLeft,
            Ordering::Greater | Ordering::Equal => R::PruneRight,
        };
    }
    if is_less_than(left.comparison_type) && is_less_than(right.comparison_type) {
        return match order {
            Ordering::Greater => R::PruneLeft,
            Ordering::Equal if left.comparison_type == E::CompareLessthanorequalto => R::PruneLeft,
            Ordering::Less | Ordering::Equal => R::PruneRight,
        };
    }
    if is_less_than(left.comparison_type) {
        return match order {
            Ordering::Greater => R::PruneNothing,
            // Equal bounds only meet if both are inclusive.
            Ordering::Equal
                if left.comparison_type == E::CompareLessthanorequalto
                    && right.comparison_type == E::CompareGreaterthanorequalto =>
            {
                R::PruneNothing
            }
            Ordering::Less | Ordering::Equal => R::UnsatisfiableCondition,
        };
    }
    invert(compare_value_information(right, left))
}

/// Combines AND-ed filters (DuckDB's `FilterCombiner`).
#[derive(Debug)]
pub struct FilterCombiner<'a> {
    context: &'a ClientContext,
    /// Filters the combiner does not reason about, in the order they came.
    remaining_filters: Vec<Expression>,
    /// The distinct expressions compared, by node id (DuckDB's `stored_expressions`).
    stored_expressions: Vec<Expression>,
    /// The equivalence set of each node, by node id (DuckDB's `equivalence_set_map`).
    equivalence_set_of: Vec<Option<usize>>,
    /// The constant comparisons of each equivalence set.
    constant_values: BTreeMap<usize, Vec<ValueInformation>>,
    /// The nodes of each equivalence set.
    equivalence_map: BTreeMap<usize, Vec<usize>>,
    set_index: usize,
}

impl<'a> FilterCombiner<'a> {
    pub fn new(context: &'a ClientContext) -> Self {
        Self {
            context,
            remaining_filters: Vec::new(),
            stored_expressions: Vec::new(),
            equivalence_set_of: Vec::new(),
            constant_values: BTreeMap::new(),
            equivalence_map: BTreeMap::new(),
            set_index: 0,
        }
    }

    /// The node id of an expression, storing a copy the first time (DuckDB's `GetNode`).
    fn get_node(&mut self, expression: &Expression) -> usize {
        if let Some(node) = self
            .stored_expressions
            .iter()
            .position(|stored| stored.equals(expression))
        {
            return node;
        }
        self.stored_expressions.push(expression.clone());
        self.equivalence_set_of.push(None);
        self.stored_expressions.len() - 1
    }

    /// The equivalence set of a node, created if needed (DuckDB's `GetEquivalenceSet`).
    fn get_equivalence_set(&mut self, node: usize) -> usize {
        if let Some(set) = self.equivalence_set_of[node] {
            return set;
        }
        let set = self.set_index;
        self.set_index += 1;
        self.equivalence_set_of[node] = Some(set);
        self.equivalence_map.insert(set, vec![node]);
        self.constant_values.insert(set, Vec::new());
        set
    }

    /// Adds a constant comparison to a set, pruning implied ones (DuckDB's
    /// `AddConstantComparison`).
    fn add_constant_comparison(
        info_list: &mut Vec<ValueInformation>,
        info: ValueInformation,
    ) -> FilterResult {
        if info.constant.is_null() {
            return FilterResult::Unsatisfiable;
        }
        let mut i = 0;
        while i < info_list.len() {
            match compare_value_information(&info_list[i], &info) {
                ValueComparisonResult::PruneLeft => {
                    info_list.remove(i);
                    continue;
                }
                ValueComparisonResult::PruneRight => return FilterResult::Success,
                ValueComparisonResult::UnsatisfiableCondition => {
                    info_list.push(info);
                    return FilterResult::Unsatisfiable;
                }
                ValueComparisonResult::PruneNothing => {}
            }
            i += 1;
        }
        info_list.push(info);
        FilterResult::Success
    }

    fn add_to_set(&mut self, set: usize, info: ValueInformation) -> FilterResult {
        let list = self.constant_values.entry(set).or_default();
        Self::add_constant_comparison(list, info)
    }

    /// Adds a filter; filters the combiner cannot use are kept as they are (DuckDB's
    /// `FilterCombiner::AddFilter(unique_ptr<Expression>)`).
    pub fn add_filter(&mut self, expression: Expression) -> Result<FilterResult> {
        let result = self.add_filter_internal(&expression)?;
        if result == FilterResult::Unsupported {
            self.remaining_filters.push(expression);
            return Ok(FilterResult::Success);
        }
        Ok(result)
    }

    /// DuckDB's `FilterCombiner::AddFilter(Expression &)`.
    fn add_filter_internal(&mut self, expression: &Expression) -> Result<FilterResult> {
        if expression.has_parameter() {
            return Ok(FilterResult::Unsupported);
        }
        if expression.is_foldable() {
            let Some(value) = try_evaluate_scalar(self.context, expression) else {
                return Ok(FilterResult::Unsupported);
            };
            return Ok(match boolean_value(&value)? {
                Some(true) => FilterResult::Success,
                _ => FilterResult::Unsatisfiable,
            });
        }
        match &expression.kind {
            ExpressionKind::Between(between) => self.add_between_filter(between),
            ExpressionKind::Comparison(comparison) => {
                self.add_bound_comparison_filter(expression.expression_type, comparison)
            }
            _ => Ok(FilterResult::Unsupported),
        }
    }

    fn add_between_filter(&mut self, between: &BoundBetweenExpression) -> Result<FilterResult> {
        let lower_is_scalar = between.lower.is_foldable();
        let upper_is_scalar = between.upper.is_foldable();
        if !lower_is_scalar && !upper_is_scalar {
            return Ok(FilterResult::Unsupported);
        }
        let node = self.get_node(&between.input);
        let set = self.get_equivalence_set(node);
        let result = if lower_is_scalar {
            let Some(constant) = try_evaluate_scalar(self.context, &between.lower) else {
                return Ok(FilterResult::Unsupported);
            };
            self.add_to_set(
                set,
                ValueInformation {
                    constant,
                    comparison_type: between.lower_comparison_type(),
                },
            )
        } else {
            let comparison = BoundComparisonExpression {
                left: between.input.clone(),
                right: between.lower.clone(),
            };
            self.add_bound_comparison_filter(between.lower_comparison_type(), &comparison)?
        };
        if result != FilterResult::Success {
            return Ok(result);
        }
        if upper_is_scalar {
            let Some(constant) = try_evaluate_scalar(self.context, &between.upper) else {
                return Ok(FilterResult::Unsupported);
            };
            Ok(self.add_to_set(
                set,
                ValueInformation {
                    constant,
                    comparison_type: between.upper_comparison_type(),
                },
            ))
        } else {
            let comparison = BoundComparisonExpression {
                left: between.input.clone(),
                right: between.upper.clone(),
            };
            self.add_bound_comparison_filter(between.upper_comparison_type(), &comparison)
        }
    }

    /// DuckDB's `AddBoundComparisonFilter`.
    fn add_bound_comparison_filter(
        &mut self,
        comparison_type: ExpressionType,
        comparison: &BoundComparisonExpression,
    ) -> Result<FilterResult> {
        if !supported_filter_comparison(comparison_type) {
            return Ok(FilterResult::Unsupported);
        }
        let left_is_scalar = comparison.left.is_foldable();
        let right_is_scalar = comparison.right.is_foldable();
        if left_is_scalar || right_is_scalar {
            let (scalar, non_scalar) = if left_is_scalar {
                (&comparison.left, &comparison.right)
            } else {
                (&comparison.right, &comparison.left)
            };
            let node = self.get_node(non_scalar);
            let set = self.get_equivalence_set(node);
            let Some(constant) = try_evaluate_scalar(self.context, scalar) else {
                return Ok(FilterResult::Unsupported);
            };
            if constant.is_null() {
                return Ok(FilterResult::Unsatisfiable);
            }
            let info = ValueInformation {
                comparison_type: if left_is_scalar {
                    flip_comparison(comparison_type)
                } else {
                    comparison_type
                },
                constant,
            };
            if self.stored_expressions[node].return_type != *info.constant.logical_type() {
                return Ok(FilterResult::Unsupported);
            }
            let result = self.add_to_set(set, info);
            if let Some(transitive) = self.find_transitive_filter(non_scalar) {
                let transitive_result = self.add_transitive_filters(&transitive, true)?;
                if transitive_result == FilterResult::Unsupported {
                    self.remaining_filters.push(transitive);
                }
                if transitive_result == FilterResult::Unsatisfiable {
                    return Ok(FilterResult::Unsatisfiable);
                }
            }
            return Ok(result);
        }
        if comparison_type != ExpressionType::CompareEqual {
            return Ok(FilterResult::Unsupported);
        }
        let left_node = self.get_node(&comparison.left);
        let right_node = self.get_node(&comparison.right);
        if left_node == right_node {
            return Ok(FilterResult::Unsupported);
        }
        let left_set = self.get_equivalence_set(left_node);
        let right_set = self.get_equivalence_set(right_node);
        if left_set == right_set {
            return Ok(FilterResult::Success);
        }
        // Merge the right set into the left one.
        let right_nodes = self.equivalence_map.remove(&right_set).unwrap_or_default();
        for &node in &right_nodes {
            self.equivalence_set_of[node] = Some(left_set);
        }
        self.equivalence_map
            .entry(left_set)
            .or_default()
            .extend(right_nodes);
        let right_constants = self.constant_values.remove(&right_set).unwrap_or_default();
        for constant in right_constants {
            if self.add_to_set(left_set, constant) == FilterResult::Unsatisfiable {
                return Ok(FilterResult::Unsatisfiable);
            }
        }
        Ok(FilterResult::Success)
    }

    /// Derives constant comparisons through a comparison of two expressions, e.g.
    /// `j > i AND i > 10` gives `j > 10` (DuckDB's `AddTransitiveFilters`).
    fn add_transitive_filters(&mut self, transitive: &Expression, is_root: bool) -> Result<FilterResult> {
        let comparison_type = transitive.expression_type;
        let ExpressionKind::Comparison(comparison) = &transitive.kind else {
            return Ok(FilterResult::Unsupported);
        };
        if !is_greater_than(comparison_type) && !is_less_than(comparison_type) {
            return Ok(FilterResult::Unsupported);
        }
        let left_node = self.get_node(&comparison.left);
        let right_node = self.get_node(&comparison.right);
        if left_node == right_node {
            return Ok(FilterResult::Unsupported);
        }
        let left_set = self.get_equivalence_set(left_node);
        let right_set = self.get_equivalence_set(right_node);
        if left_set == right_set {
            if matches!(
                comparison_type,
                ExpressionType::CompareGreaterthan | ExpressionType::CompareLessthan
            ) {
                return Ok(FilterResult::Unsatisfiable);
            }
            return Ok(FilterResult::Success);
        }
        let right_constants = self.constant_values.get(&right_set).cloned().unwrap_or_default();
        let mut is_successful = false;
        let mut is_inserted = false;
        for right_constant in right_constants {
            let info_type = if right_constant.comparison_type == ExpressionType::CompareEqual {
                comparison_type
            } else if (comparison_type == ExpressionType::CompareGreaterthanorequalto
                && is_greater_than(right_constant.comparison_type))
                || (comparison_type == ExpressionType::CompareLessthanorequalto
                    && is_less_than(right_constant.comparison_type))
            {
                if !is_inserted {
                    self.remaining_filters.push(transitive.clone());
                    is_inserted = true;
                }
                right_constant.comparison_type
            } else if (comparison_type == ExpressionType::CompareGreaterthan
                && is_greater_than(right_constant.comparison_type))
                || (comparison_type == ExpressionType::CompareLessthan
                    && is_less_than(right_constant.comparison_type))
            {
                if !is_inserted {
                    self.remaining_filters.push(transitive.clone());
                    is_inserted = true;
                }
                comparison_type
            } else {
                continue;
            };
            let info = ValueInformation {
                constant: right_constant.constant,
                comparison_type: info_type,
            };
            if self.add_to_set(left_set, info) == FilterResult::Unsatisfiable {
                return Ok(FilterResult::Unsatisfiable);
            }
            is_successful = true;
        }
        if !is_successful {
            return Ok(FilterResult::Unsupported);
        }
        if is_root && let Some(next) = self.find_transitive_filter(&comparison.left) {
            let result = self.add_transitive_filters(&next, false)?;
            if result == FilterResult::Unsupported {
                self.remaining_filters.push(next);
            }
            if result == FilterResult::Unsatisfiable {
                return Ok(FilterResult::Unsatisfiable);
            }
        }
        Ok(FilterResult::Success)
    }

    /// Takes a remaining comparison whose right side is `expression` out of the remaining
    /// filters (DuckDB's `FindTransitiveFilter`).
    fn find_transitive_filter(&mut self, expression: &Expression) -> Option<Expression> {
        if !matches!(expression.kind, ExpressionKind::ColumnRef(_)) {
            return None;
        }
        let position = self.remaining_filters.iter().position(|filter| {
            matches!(&filter.kind, ExpressionKind::Comparison(comparison)
                if expression.equals(&comparison.right)
                    && filter.expression_type != ExpressionType::CompareNotequal)
        })?;
        Some(self.remaining_filters.remove(position))
    }

    /// Hands out the combined filters and resets the combiner (DuckDB's
    /// `GenerateFilters`).
    pub fn generate_filters(&mut self, mut callback: impl FnMut(Expression)) {
        for filter in std::mem::take(&mut self.remaining_filters) {
            callback(filter);
        }
        let stored = std::mem::take(&mut self.stored_expressions);
        let mut constant_values = std::mem::take(&mut self.constant_values);
        for (set, nodes) in std::mem::take(&mut self.equivalence_map) {
            let constants = constant_values.remove(&set).unwrap_or_default();
            for (i, &node) in nodes.iter().enumerate() {
                let entry = &stored[node];
                for &other in &nodes[i + 1..] {
                    callback(Expression::comparison(
                        ExpressionType::CompareEqual,
                        entry.clone(),
                        stored[other].clone(),
                    ));
                }
                let mut lower: Option<usize> = None;
                let mut upper: Option<usize> = None;
                for (k, info) in constants.iter().enumerate() {
                    if is_greater_than(info.comparison_type) {
                        lower = Some(k);
                    } else if is_less_than(info.comparison_type) {
                        upper = Some(k);
                    } else {
                        callback(Expression::comparison(
                            info.comparison_type,
                            entry.clone(),
                            Expression::constant(info.constant.clone()),
                        ));
                    }
                }
                match (lower, upper) {
                    (Some(lower), Some(upper)) => {
                        let (lower, upper) = (&constants[lower], &constants[upper]);
                        callback(Expression::new(
                            ExpressionType::CompareBetween,
                            LogicalType::BOOLEAN,
                            ExpressionKind::Between(Box::new(BoundBetweenExpression {
                                input: entry.clone(),
                                lower: Expression::constant(lower.constant.clone()),
                                upper: Expression::constant(upper.constant.clone()),
                                lower_inclusive: lower.comparison_type
                                    == ExpressionType::CompareGreaterthanorequalto,
                                upper_inclusive: upper.comparison_type
                                    == ExpressionType::CompareLessthanorequalto,
                            })),
                        ));
                    }
                    (Some(index), None) | (None, Some(index)) => {
                        let info = &constants[index];
                        callback(Expression::comparison(
                            info.comparison_type,
                            entry.clone(),
                            Expression::constant(info.constant.clone()),
                        ));
                    }
                    (None, None) => {}
                }
            }
        }
        self.equivalence_set_of.clear();
        self.set_index = 0;
    }

    /// Whether the combiner holds no filters.
    pub fn is_empty(&self) -> bool {
        self.remaining_filters.is_empty() && self.equivalence_map.is_empty()
    }

    /// Turns the filters on columns of a scan into table filters keyed by column index
    /// (DuckDB's `GenerateTableScanFilters`). `pushdown_results` gets the result of each
    /// remaining filter that was not pushed down completely.
    pub fn generate_table_scan_filters(
        &mut self,
        column_ids: &[ColumnIndex],
        pushdown_results: &mut Vec<FilterPushdownResult>,
        supported: &dyn Fn(usize, &TableFilter) -> bool,
    ) -> Result<TableFilterSet> {
        let mut table_filters = TableFilterSet::default();
        let sets: Vec<usize> = self.constant_values.keys().copied().collect();
        for set in sets {
            self.try_pushdown_constant_filter(&mut table_filters, column_ids, set, supported);
        }
        let mut index = 0;
        while index < self.remaining_filters.len() {
            let mut candidates = TableFilterSet::default();
            let mut result =
                try_pushdown_expression(&mut candidates, column_ids, &self.remaining_filters[index]);
            if candidates
                .filters
                .iter()
                .all(|(&column, filter)| supported(column, filter))
            {
                for (column, filter) in candidates.filters {
                    table_filters.push_filter(column, filter);
                }
            } else {
                result = FilterPushdownResult::NoPushdown;
            }
            if result == FilterPushdownResult::PushedDownFully {
                self.remaining_filters.remove(index);
            } else {
                pushdown_results.push(result);
                index += 1;
            }
        }
        Ok(table_filters)
    }

    /// DuckDB's `TryPushdownConstantFilter`.
    fn try_pushdown_constant_filter(
        &mut self,
        table_filters: &mut TableFilterSet,
        column_ids: &[ColumnIndex],
        set: usize,
        supported: &dyn Fn(usize, &TableFilter) -> bool,
    ) {
        let Some(info_list) = self.constant_values.get(&set) else {
            return;
        };
        let Some(first) = info_list.first() else {
            return;
        };
        if !type_supports_constant_filter(first.constant.logical_type())
            || !supported_filter_comparison(first.comparison_type)
        {
            return;
        }
        let Some(nodes) = self.equivalence_map.get(&set) else {
            return;
        };
        let [node] = nodes.as_slice() else {
            return;
        };
        let expression = &self.stored_expressions[*node];
        let Some(column) = bound_column_index(column_ids, expression) else {
            return;
        };
        let mut candidates = Vec::new();
        for info in info_list {
            let Some(op) = compare_op(info.comparison_type) else {
                continue;
            };
            let filter = TableFilter::ConstantComparison {
                op,
                constant: info.constant.clone(),
            };
            candidates.push(push_down_filter_into_expression(expression, filter));
        }
        if !candidates.iter().all(|filter| supported(column, filter)) {
            return;
        }
        for filter in candidates {
            table_filters.push_filter(column, filter);
        }
        self.equivalence_map.remove(&set);
    }
}

/// Numeric, VARCHAR and BOOLEAN constants make table filters (DuckDB's
/// `TypeSupportsConstantFilter`).
fn type_supports_constant_filter(ty: &LogicalType) -> bool {
    let physical = ty.physical_type();
    ty.is_numeric()
        || matches!(
            physical,
            PhysicalType::Varchar
                | PhysicalType::Bool
                | PhysicalType::Int8
                | PhysicalType::Int16
                | PhysicalType::Int32
                | PhysicalType::Int64
                | PhysicalType::Int128
                | PhysicalType::UInt8
                | PhysicalType::UInt16
                | PhysicalType::UInt32
                | PhysicalType::UInt64
                | PhysicalType::UInt128
                | PhysicalType::Float
                | PhysicalType::Double
        )
}

/// The table filter key of a scan column; `None` for a pushed-down extract, which
/// filters cannot be pushed into yet (DuckDB's FIXMEs on `IsPushdownExtract`).
fn filter_key(column: &ColumnIndex) -> Option<usize> {
    if column.is_pushdown_extract() {
        return None;
    }
    Some(usize::try_from(column.index).unwrap_or(usize::MAX))
}

/// The scanned column a column reference, possibly inside `struct_extract` calls, reads
/// (DuckDB's `TryGetBoundColumnIndex`).
fn bound_column_index(column_ids: &[ColumnIndex], expression: &Expression) -> Option<usize> {
    match &expression.kind {
        ExpressionKind::ColumnRef(column) => column_ids
            .get(usize::try_from(column.binding.column_index).ok()?)
            .and_then(filter_key),
        ExpressionKind::Function(function)
            if matches!(&*function.function.name, b"struct_extract" | b"struct_extract_at") =>
        {
            bound_column_index(column_ids, function.children.first()?)
        }
        _ => None,
    }
}

/// Wraps a filter on a struct field into struct filters down to the column (DuckDB's
/// `PushDownFilterIntoExpr`).
fn push_down_filter_into_expression(expression: &Expression, inner: TableFilter) -> TableFilter {
    let ExpressionKind::Function(function) = &expression.kind else {
        return inner;
    };
    let (Some(child), Some(field)) = (function.children.first(), function.children.get(1)) else {
        return inner;
    };
    let field = field.as_constant();
    match (&*function.function.name, field) {
        (b"struct_extract", Some(name)) => {
            let name = rustdb_common::ByteString::from(name.as_bytes().unwrap_or_default());
            let index = child
                .return_type
                .struct_children()
                .and_then(|children| {
                    children
                        .iter()
                        .position(|(child_name, _)| child_name.eq_ignore_ascii_case(&name))
                })
                .unwrap_or(0);
            let filter = TableFilter::StructExtract {
                child_index: index,
                child_name: name,
                child_filter: Box::new(inner),
            };
            push_down_filter_into_expression(child, filter)
        }
        (b"struct_extract_at", Some(position)) => {
            let index = position
                .as_i128()
                .and_then(|position| usize::try_from(position - 1).ok())
                .unwrap_or(0);
            let filter = TableFilter::StructExtract {
                child_index: index,
                child_name: rustdb_common::ByteString::new(),
                child_filter: Box::new(inner),
            };
            push_down_filter_into_expression(child, filter)
        }
        _ => inner,
    }
}

/// The column reference and constant of `function(column, constant)`.
fn column_and_constant<'e>(
    column_ids: &[ColumnIndex],
    children: &'e [Expression],
) -> Option<(usize, &'e Value)> {
    let ExpressionKind::ColumnRef(column) = &children.first()?.kind else {
        return None;
    };
    let constant = children.get(1)?.as_constant()?;
    let column = column_ids.get(usize::try_from(column.binding.column_index).ok()?)?;
    Some((filter_key(column)?, constant))
}

/// The next string after every string starting with `prefix` (DuckDB's
/// `FindNextLegalUTF8`): the last code point incremented.
pub fn find_next_legal_utf8(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    let last = chars.pop()?;
    let mut next = u32::from(last) + 1;
    if (0xD800..=0xDFFF).contains(&next) {
        next = 0xE000;
    }
    chars.push(char::from_u32(next)?);
    Some(chars.into_iter().collect())
}

/// `prefix(column, 'literal')` as a range on the column (DuckDB's `TryPushdownPrefixFilter`).
fn try_pushdown_prefix_filter(
    table_filters: &mut TableFilterSet,
    column_ids: &[ColumnIndex],
    expression: &Expression,
) -> FilterPushdownResult {
    let ExpressionKind::Function(function) = &expression.kind else {
        return FilterPushdownResult::NoPushdown;
    };
    if &*function.function.name != b"prefix" {
        return FilterPushdownResult::NoPushdown;
    }
    let Some((column, constant)) = column_and_constant(column_ids, &function.children) else {
        return FilterPushdownResult::NoPushdown;
    };
    let Some(prefix) = constant.as_str() else {
        return FilterPushdownResult::NoPushdown;
    };
    if prefix.is_empty() {
        return FilterPushdownResult::NoPushdown;
    }
    table_filters.push_filter(
        column,
        TableFilter::ConstantComparison {
            op: CompareOp::GreaterThanOrEqual,
            constant: Value::varchar(prefix),
        },
    );
    match find_next_legal_utf8(prefix) {
        Some(upper) => {
            table_filters.push_filter(
                column,
                TableFilter::ConstantComparison {
                    op: CompareOp::LessThan,
                    constant: Value::varchar(upper),
                },
            );
            FilterPushdownResult::PushedDownFully
        }
        None => FilterPushdownResult::NoPushdown,
    }
}

/// `column LIKE 'literal%...'` as equality or a prefix range (DuckDB's
/// `TryPushdownLikeFilter`).
fn try_pushdown_like_filter(
    table_filters: &mut TableFilterSet,
    column_ids: &[ColumnIndex],
    expression: &Expression,
) -> FilterPushdownResult {
    let ExpressionKind::Function(function) = &expression.kind else {
        return FilterPushdownResult::NoPushdown;
    };
    if &*function.function.name != b"~~" {
        return FilterPushdownResult::NoPushdown;
    }
    let Some((column, constant)) = column_and_constant(column_ids, &function.children) else {
        return FilterPushdownResult::NoPushdown;
    };
    if constant.is_null() {
        table_filters.push_filter(column, TableFilter::IsNotNull);
        return FilterPushdownResult::PushedDownFully;
    }
    let Some(pattern) = constant.as_bytes() else {
        return FilterPushdownResult::NoPushdown;
    };
    if matches!(pattern.first(), Some(b'%' | b'_')) {
        return FilterPushdownResult::NoPushdown;
    }
    let wildcard = pattern.iter().position(|&c| c == b'%' || c == b'_');
    let Some(end) = wildcard else {
        table_filters.push_filter(
            column,
            TableFilter::ConstantComparison {
                op: CompareOp::Equal,
                constant: Value::bytes(constant.logical_type().clone(), pattern),
            },
        );
        return FilterPushdownResult::PushedDownFully;
    };
    let prefix = pattern[..end].to_vec();
    let mut upper = prefix.clone();
    if let Some(last) = upper.last_mut() {
        *last = last.wrapping_add(1);
    }
    table_filters.push_filter(
        column,
        TableFilter::ConstantComparison {
            op: CompareOp::GreaterThanOrEqual,
            constant: Value::bytes(LogicalType::VARCHAR, &prefix),
        },
    );
    table_filters.push_filter(
        column,
        TableFilter::ConstantComparison {
            op: CompareOp::LessThan,
            constant: Value::bytes(LogicalType::VARCHAR, &upper),
        },
    );
    FilterPushdownResult::PushedDownPartially
}

/// Whether integral values are consecutive; sorts them (DuckDB's `IsDenseRange`).
fn is_dense_range(values: &mut [Value]) -> bool {
    let Some(first) = values.first() else {
        return true;
    };
    if !first.logical_type().is_integral() || first.logical_type().id() == LogicalTypeId::UHugeInt {
        return false;
    }
    values.sort_by(|left, right| compare_values(left, right).unwrap_or(Ordering::Equal));
    let mut previous = values[0].as_i128();
    for value in &values[1..] {
        let current = value.as_i128();
        match (previous, current) {
            (Some(previous), Some(current)) if current.checked_sub(previous) == Some(1) => {}
            _ => return false,
        }
        previous = current;
    }
    true
}

/// `column IN (constants)` as an equality, a range, or a zone-map-only IN filter
/// (DuckDB's `TryPushdownInFilter`).
fn try_pushdown_in_filter(
    table_filters: &mut TableFilterSet,
    column_ids: &[ColumnIndex],
    expression: &Expression,
) -> FilterPushdownResult {
    if expression.expression_type != ExpressionType::CompareIn {
        return FilterPushdownResult::NoPushdown;
    }
    let ExpressionKind::Operator(operator) = &expression.kind else {
        return FilterPushdownResult::NoPushdown;
    };
    let Some(ExpressionKind::ColumnRef(column)) = operator.children.first().map(|child| &child.kind) else {
        return FilterPushdownResult::NoPushdown;
    };
    let Some(column) = usize::try_from(column.binding.column_index)
        .ok()
        .and_then(|index| column_ids.get(index))
        .and_then(filter_key)
    else {
        return FilterPushdownResult::NoPushdown;
    };
    let mut values = Vec::with_capacity(operator.children.len() - 1);
    for child in &operator.children[1..] {
        match child.as_constant() {
            Some(value) if !value.is_null() && child.expression_type == ExpressionType::ValueConstant => {
                values.push(value.clone());
            }
            _ => return FilterPushdownResult::NoPushdown,
        }
    }
    let Some(first) = values.first() else {
        return FilterPushdownResult::NoPushdown;
    };
    let ty = first.logical_type().clone();
    if values.len() == 1 && type_supports_constant_filter(&ty) {
        table_filters.push_filter(
            column,
            TableFilter::ConstantComparison {
                op: CompareOp::Equal,
                constant: values.remove(0),
            },
        );
        return FilterPushdownResult::PushedDownFully;
    }
    if ty.is_integral() && is_dense_range(&mut values) {
        let (Some(min), Some(max)) = (values.first().cloned(), values.last().cloned()) else {
            return FilterPushdownResult::NoPushdown;
        };
        table_filters.push_filter(
            column,
            TableFilter::ConstantComparison {
                op: CompareOp::GreaterThanOrEqual,
                constant: min,
            },
        );
        table_filters.push_filter(
            column,
            TableFilter::ConstantComparison {
                op: CompareOp::LessThanOrEqual,
                constant: max,
            },
        );
        return FilterPushdownResult::PushedDownFully;
    }
    table_filters.push_filter(
        column,
        TableFilter::Optional(Some(Box::new(TableFilter::In(values)))),
    );
    FilterPushdownResult::PushedDownPartially
}

/// An OR of comparisons of one column with constants as a zone-map-only filter (DuckDB's
/// `TryPushdownOrClause`).
fn try_pushdown_or_clause(
    table_filters: &mut TableFilterSet,
    column_ids: &[ColumnIndex],
    expression: &Expression,
) -> FilterPushdownResult {
    if expression.expression_type != ExpressionType::ConjunctionOr {
        return FilterPushdownResult::NoPushdown;
    }
    let ExpressionKind::Conjunction(conjunction) = &expression.kind else {
        return FilterPushdownResult::NoPushdown;
    };
    if conjunction.children.is_empty() {
        return FilterPushdownResult::NoPushdown;
    }
    let mut column_id = None;
    let mut children = Vec::new();
    for child in &conjunction.children {
        let ExpressionKind::Comparison(comparison) = &child.kind else {
            return FilterPushdownResult::NoPushdown;
        };
        let (column, constant, invert) = match (&comparison.left.kind, &comparison.right.kind) {
            (ExpressionKind::ColumnRef(column), ExpressionKind::Constant(constant)) => {
                (column, constant, false)
            }
            (ExpressionKind::Constant(constant), ExpressionKind::ColumnRef(column)) => {
                (column, constant, true)
            }
            _ => return FilterPushdownResult::NoPushdown,
        };
        let Some(id) = usize::try_from(column.binding.column_index)
            .ok()
            .and_then(|index| column_ids.get(index))
            .and_then(filter_key)
        else {
            return FilterPushdownResult::NoPushdown;
        };
        match column_id {
            None => column_id = Some(id),
            Some(existing) if existing != id => return FilterPushdownResult::NoPushdown,
            Some(_) => {}
        }
        let comparison_type = if invert {
            flip_comparison(child.expression_type)
        } else {
            child.expression_type
        };
        if constant.value.is_null() {
            match comparison_type {
                ExpressionType::CompareDistinctFrom => children.push(TableFilter::IsNotNull),
                ExpressionType::CompareNotDistinctFrom => children.push(TableFilter::IsNull),
                _ => {}
            }
        } else if let Some(op) = compare_op(comparison_type) {
            children.push(TableFilter::ConstantComparison {
                op,
                constant: constant.value.clone(),
            });
        } else {
            return FilterPushdownResult::NoPushdown;
        }
    }
    let Some(column) = column_id else {
        return FilterPushdownResult::NoPushdown;
    };
    table_filters.push_filter(
        column,
        TableFilter::Optional(Some(Box::new(TableFilter::Or(children)))),
    );
    FilterPushdownResult::PushedDownPartially
}

/// Pushes a non-volatile predicate on a single scanned column into the scan as an
/// expression filter (DuckDB's `TryPushdownGenericExpression`).
pub fn try_pushdown_generic_expression(
    get: &mut LogicalGet,
    expression: &Expression,
) -> FilterPushdownResult {
    let bindings = crate::expression_util::column_bindings(expression);
    let Some(first) = bindings.first() else {
        return FilterPushdownResult::NoPushdown;
    };
    if bindings.iter().any(|binding| binding != first) {
        return FilterPushdownResult::NoPushdown;
    }
    let Some(column) = get
        .column_ids
        .get(first.column_index as usize)
        .and_then(filter_key)
    else {
        return FilterPushdownResult::NoPushdown;
    };
    let mut filter = expression.clone();
    crate::expression_util::replace_column_refs(&mut filter, &mut |column| {
        Some(Expression::reference(
            column.alias.clone(),
            column.return_type.clone(),
            0,
        ))
    });
    let filter = TableFilter::Expression(Arc::new(ExpressionTableFilter { expression: filter }));
    if let (Some(supported), Some(data)) = (&get.function.filter_support, &get.bind_data)
        && !supported(data.as_ref(), column, &filter)
    {
        return FilterPushdownResult::NoPushdown;
    }
    get.table_filters.push_filter(column, filter);
    FilterPushdownResult::PushedDownFully
}

/// DuckDB's `TryPushdownExpression`.
fn try_pushdown_expression(
    table_filters: &mut TableFilterSet,
    column_ids: &[ColumnIndex],
    expression: &Expression,
) -> FilterPushdownResult {
    for attempt in [
        try_pushdown_prefix_filter,
        try_pushdown_like_filter,
        try_pushdown_in_filter,
        try_pushdown_or_clause,
    ] {
        let result = attempt(table_filters, column_ids, expression);
        if result != FilterPushdownResult::NoPushdown {
            return result;
        }
    }
    FilterPushdownResult::NoPushdown
}
