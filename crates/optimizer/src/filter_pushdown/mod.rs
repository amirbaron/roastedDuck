//! Filter pushdown (DuckDB's `FilterPushdown`): filters move down the plan as far as
//! they can go, through projections, aggregates and joins, into the inputs they refer
//! to and finally into table scans. Filters over both sides of a cross product turn it
//! into a join.

mod aggregate;
mod get;
mod join;
mod projection;
mod set_operation;
mod window;

use std::collections::BTreeSet;

use rustdb_ast::JoinType;
use rustdb_common::Result;
use rustdb_planner::operator::split_predicates;
use rustdb_planner::{BoundLimitNode, Expression, LogicalKind, LogicalOperator, LogicalOperatorType};

use crate::expression_util::{enumerate_expression, table_indexes};
use crate::filter_combiner::{FilterCombiner, FilterResult};
use crate::optimizer::Optimizer;

/// A filter and the tables it references (DuckDB's `FilterPushdown::Filter`).
#[derive(Debug)]
pub struct Filter {
    pub bindings: BTreeSet<u64>,
    pub filter: Expression,
}

impl Filter {
    pub fn new(filter: Expression) -> Self {
        let bindings = table_indexes(&filter).into_iter().collect();
        Self { bindings, filter }
    }

    /// Recomputes the referenced tables (DuckDB's `ExtractBindings`).
    pub fn extract_bindings(&mut self) {
        self.bindings = table_indexes(&self.filter).into_iter().collect();
    }
}

/// The table indexes of the columns an operator produces (DuckDB's
/// `LogicalJoin::GetTableReferences`).
pub fn table_references(op: &LogicalOperator) -> BTreeSet<u64> {
    op.column_bindings()
        .iter()
        .map(|binding| binding.table_index)
        .collect()
}

/// An empty result with the columns of `op` (DuckDB's `LogicalEmptyResult(op)`).
pub fn empty_result(mut op: LogicalOperator) -> Result<LogicalOperator> {
    let bindings = op.column_bindings();
    op.resolve_operator_types()?;
    Ok(LogicalOperator::new(LogicalKind::EmptyResult {
        return_types: op.types,
        bindings,
    }))
}

/// Pushes filters down one subtree (DuckDB's `FilterPushdown`).
#[derive(Debug)]
pub struct FilterPushdown<'a> {
    optimizer: &'a Optimizer,
    combiner: FilterCombiner<'a>,
    convert_mark_joins: bool,
    filters: Vec<Filter>,
}

impl<'a> FilterPushdown<'a> {
    pub fn new(optimizer: &'a Optimizer, convert_mark_joins: bool) -> Self {
        Self {
            optimizer,
            combiner: FilterCombiner::new(optimizer.context()),
            convert_mark_joins,
            filters: Vec::new(),
        }
    }

    /// A pushdown for a child, with the same settings.
    fn child(&self) -> Self {
        Self::new(self.optimizer, self.convert_mark_joins)
    }

    /// Clears `convert_mark_to_semi` of MARK joins whose marker is used above them
    /// (DuckDB's `FilterPushdown::CheckMarkToSemi`).
    pub fn check_mark_to_semi(op: &mut LogicalOperator, table_bindings: &mut BTreeSet<u64>) {
        match &mut op.kind {
            LogicalKind::ComparisonJoin(join) => {
                if join.join_type == JoinType::Mark && table_bindings.contains(&join.mark_index) {
                    join.convert_mark_to_semi = false;
                }
            }
            LogicalKind::Projection { .. } => {
                let mut new_bindings = BTreeSet::new();
                for expression in &op.expressions {
                    collect_column_tables(expression, &mut new_bindings);
                }
                if !op.expressions.is_empty() {
                    *table_bindings = new_bindings;
                }
            }
            LogicalKind::Aggregate(aggregate) => {
                let mut new_bindings = BTreeSet::new();
                for expression in aggregate.groups.iter().chain(&op.expressions) {
                    collect_column_tables(expression, &mut new_bindings);
                }
                *table_bindings = new_bindings;
            }
            _ => {}
        }
        for child in &mut op.children {
            Self::check_mark_to_semi(child, table_bindings);
        }
    }

    /// Pushes the collected filters into `op` (DuckDB's `FilterPushdown::Rewrite`).
    pub fn rewrite(&mut self, op: LogicalOperator) -> Result<LogicalOperator> {
        match op.operator_type() {
            LogicalOperatorType::LogicalAggregateAndGroupBy => self.pushdown_aggregate(op),
            LogicalOperatorType::LogicalFilter => self.pushdown_filter(op),
            LogicalOperatorType::LogicalCrossProduct => self.pushdown_cross_product(op),
            LogicalOperatorType::LogicalComparisonJoin
            | LogicalOperatorType::LogicalAnyJoin
            | LogicalOperatorType::LogicalAsofJoin
            | LogicalOperatorType::LogicalDelimJoin => self.pushdown_join(op),
            LogicalOperatorType::LogicalProjection => self.pushdown_projection(op),
            LogicalOperatorType::LogicalIntersect
            | LogicalOperatorType::LogicalExcept
            | LogicalOperatorType::LogicalUnion => self.pushdown_set_operation(op),
            LogicalOperatorType::LogicalDistinct => self.pushdown_distinct(op),
            LogicalOperatorType::LogicalOrderBy => {
                let mut op = op;
                op.children = self.rewrite_children_with_self(std::mem::take(&mut op.children))?;
                Ok(op)
            }
            LogicalOperatorType::LogicalMaterializedCte => {
                let mut op = op;
                let mut children = std::mem::take(&mut op.children).into_iter();
                if let (Some(definition), Some(query)) = (children.next(), children.next()) {
                    let definition = self.child().rewrite(definition)?;
                    let query = self.rewrite(query)?;
                    op.children = vec![definition, query];
                }
                Ok(op)
            }
            LogicalOperatorType::LogicalGet => self.pushdown_get(op),
            LogicalOperatorType::LogicalLimit => self.pushdown_limit(op),
            LogicalOperatorType::LogicalWindow => self.pushdown_window(op),
            LogicalOperatorType::LogicalUnnest => self.pushdown_unnest(op),
            _ => self.finish_pushdown(op),
        }
    }

    /// Rewrites the single child of an operator with this pushdown's filters.
    fn rewrite_children_with_self(&mut self, children: Vec<LogicalOperator>) -> Result<Vec<LogicalOperator>> {
        children.into_iter().map(|child| self.rewrite(child)).collect()
    }

    /// Moves the filters into the combiner (DuckDB's `PushFilters`).
    fn push_filters(&mut self) -> Result<FilterResult> {
        for filter in std::mem::take(&mut self.filters) {
            if self.combiner.add_filter(filter.filter)? == FilterResult::Unsatisfiable {
                return Ok(FilterResult::Unsatisfiable);
            }
        }
        Ok(FilterResult::Success)
    }

    /// Adds a filter, split at its ANDs (DuckDB's `FilterPushdown::AddFilter`).
    pub fn add_filter(&mut self, expression: Expression) -> Result<FilterResult> {
        if self.push_filters()? == FilterResult::Unsatisfiable {
            return Ok(FilterResult::Unsatisfiable);
        }
        let mut expressions = vec![expression];
        split_predicates(&mut expressions);
        for expression in expressions {
            if self.combiner.add_filter(expression)? == FilterResult::Unsatisfiable {
                return Ok(FilterResult::Unsatisfiable);
            }
        }
        Ok(FilterResult::Success)
    }

    /// Takes the combined filters out of the combiner (DuckDB's `GenerateFilters`).
    pub fn generate_filters(&mut self) {
        if !self.filters.is_empty() {
            return;
        }
        let mut generated = Vec::new();
        self.combiner
            .generate_filters(|filter| generated.push(Filter::new(filter)));
        self.filters = generated;
    }

    /// A filter on top of `op`, unless there are no expressions (DuckDB's
    /// `AddLogicalFilter`).
    fn add_logical_filter(op: LogicalOperator, expressions: Vec<Expression>) -> LogicalOperator {
        if expressions.is_empty() {
            return op;
        }
        let estimated_cardinality = op.estimated_cardinality;
        let mut filter = LogicalOperator::new(LogicalKind::Filter {
            projection_map: Vec::new(),
        })
        .with_expressions(expressions)
        .with_children(vec![op]);
        filter.estimated_cardinality = estimated_cardinality;
        filter
    }

    /// The remaining filters on top of `op` (DuckDB's `PushFinalFilters`).
    fn push_final_filters(&mut self, op: LogicalOperator) -> LogicalOperator {
        let expressions = std::mem::take(&mut self.filters)
            .into_iter()
            .map(|filter| filter.filter)
            .collect();
        Self::add_logical_filter(op, expressions)
    }

    /// Pushes down into the children with fresh pushdowns and puts the filters on top
    /// (DuckDB's `FinishPushdown`).
    fn finish_pushdown(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        op.children = std::mem::take(&mut op.children)
            .into_iter()
            .map(|child| self.child().rewrite(child))
            .collect::<Result<_>>()?;
        Ok(self.push_final_filters(op))
    }

    /// DuckDB's `PushdownFilter`: the filter's expressions join the pushdown.
    fn pushdown_filter(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        if let LogicalKind::Filter { projection_map } = &op.kind
            && !projection_map.is_empty()
        {
            return self.finish_pushdown(op);
        }
        for expression in std::mem::take(&mut op.expressions) {
            if self.add_filter(expression)? == FilterResult::Unsatisfiable {
                return empty_result(op);
            }
        }
        self.generate_filters();
        let child = op
            .children
            .pop()
            .ok_or_else(|| rustdb_common::Error::internal("filter without input"))?;
        self.rewrite(child)
    }

    /// DuckDB's `PushdownDistinct`: filters pass through a plain DISTINCT.
    fn pushdown_distinct(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        if let LogicalKind::Distinct { order_by: None, .. } = &op.kind {
            op.children = self.rewrite_children_with_self(std::mem::take(&mut op.children))?;
            return Ok(op);
        }
        self.finish_pushdown(op)
    }

    /// DuckDB's `PushdownLimit`: `LIMIT 0` is empty.
    fn pushdown_limit(&mut self, op: LogicalOperator) -> Result<LogicalOperator> {
        if let LogicalKind::Limit {
            limit: BoundLimitNode::ConstantValue(0),
            ..
        } = &op.kind
        {
            return empty_result(op);
        }
        self.finish_pushdown(op)
    }

    /// The pushdown of a delim join: each filter goes into the first child whose tables
    /// cover it (DuckDB's `PushFiltersIntoDelimJoin`).
    fn push_filters_into_delim_join(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        let mut index = 0;
        while index < self.filters.len() {
            let mut pushed = false;
            for child_index in 0..op.children.len() {
                let child_tables = table_references(&op.children[child_index]);
                if !self.filters[index]
                    .bindings
                    .iter()
                    .all(|table| child_tables.contains(table))
                {
                    continue;
                }
                let mut pushdown = self.child();
                if pushdown.add_filter(self.filters[index].filter.clone())? == FilterResult::Unsatisfiable {
                    return empty_result(op);
                }
                pushdown.generate_filters();
                let child = std::mem::replace(
                    &mut op.children[child_index],
                    LogicalOperator::new(LogicalKind::DummyScan { table_index: 0 }),
                );
                op.children[child_index] = pushdown.rewrite(child)?;
                pushed = true;
                break;
            }
            if pushed {
                self.filters.remove(index);
            } else {
                index += 1;
            }
        }
        Ok(op)
    }

    /// DuckDB's `PushdownUnnest`: filters not on the unnested values go below it.
    fn pushdown_unnest(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        let LogicalKind::Unnest { unnest_index } = op.kind else {
            return self.finish_pushdown(op);
        };
        let mut child_pushdown = self.child();
        let mut remain_expressions = Vec::new();
        for filter in std::mem::take(&mut self.filters) {
            if filter.bindings.contains(&unnest_index) {
                remain_expressions.push(filter.filter);
            } else if child_pushdown.add_filter(filter.filter)? == FilterResult::Unsatisfiable {
                return empty_result(op);
            }
        }
        child_pushdown.generate_filters();
        let children = std::mem::take(&mut op.children);
        op.children = child_pushdown.rewrite_children_with_self(children)?;
        if op
            .children
            .first()
            .is_some_and(|child| matches!(child.kind, LogicalKind::EmptyResult { .. }))
        {
            return empty_result(op);
        }
        Ok(Self::add_logical_filter(op, remain_expressions))
    }
}

/// Adds the table indexes of the column references in `expression`.
fn collect_column_tables(expression: &Expression, tables: &mut BTreeSet<u64>) {
    enumerate_expression(expression, &mut |child| {
        if let Some(column) = child.as_column_ref() {
            tables.insert(column.binding.table_index);
        }
    });
}
