//! Flattening of dependent joins (DuckDB's `flatten_dependent_join.cpp`, after
//! "Unnesting Arbitrary Subqueries"): the duplicate-eliminated correlated columns of the
//! left side are pushed into the right side through `DELIM_GET` scans until no correlated
//! expressions remain, and the dependent join becomes a DELIM join.

use std::collections::HashMap;

use rustdb_ast::{JoinType, SubqueryType, WindowBoundary, WindowExcludeMode};
use rustdb_catalog::{CatalogType, EntryKind};
use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, Value};

use super::rewrite::{
    CorrelatedMap, CteScanRewriteMode, has_correlated_expressions, rewrite_correlated_expressions,
    rewrite_count_aggregates, rewrite_cte_scan,
};
use crate::binder::expression::push_collation;
use crate::binder::{Binder, CorrelatedColumns};
use crate::expression::{AggregateType, BoundOrderByNode, BoundWindowExpression, Expression, ExpressionKind};
use crate::function_binder::{FunctionBinder, add_cast_to_type};
use crate::operator::{
    BoundLimitNode, JoinCondition, LogicalComparisonJoin, LogicalDependentJoin, LogicalKind, LogicalOperator,
};
use crate::{ColumnBinding, ExpressionType, LogicalOperatorType};

/// What the flattening knows about a CTE (DuckDB keeps pointers to the CTE operators in
/// `Binder::recursive_ctes`).
#[derive(Clone, Debug)]
struct CteNode {
    node_id: u64,
    recursive: bool,
    correlated_columns: CorrelatedColumns,
}

/// One flattening pass for the correlated columns of one dependent join (DuckDB's
/// `FlattenDependentJoins`).
struct FlattenDependentJoins<'a> {
    binder: &'a Binder,
    /// Whether operators (by node id) reference the correlated columns, themselves or
    /// below.
    has_correlated_expressions: HashMap<u64, bool>,
    correlated_map: CorrelatedMap,
    base_binding: ColumnBinding,
    delim_offset: u64,
    data_offset: u64,
    correlated_columns: CorrelatedColumns,
    delim_types: Vec<LogicalType>,
    perform_delim: bool,
    any_join: bool,
    /// Whether this pass runs inside the flattening of an enclosing dependent join.
    has_parent: bool,
    /// COUNT aggregates whose references must turn NULL into 0.
    replacement_map: HashMap<ColumnBinding, u64>,
    /// The CTEs seen so far, by table index (shared by nested passes).
    ctes: HashMap<u64, CteNode>,
}

fn column_ref(name: &str, ty: &LogicalType, binding: ColumnBinding) -> Expression {
    Expression::column_ref(name, ty.clone(), binding, 0)
}

fn delim_get(table_index: u64, types: &[LogicalType]) -> LogicalOperator {
    LogicalOperator::new(LogicalKind::DelimGet {
        table_index,
        chunk_types: types.to_vec(),
    })
}

fn comparison_join(
    join_type: JoinType,
    conditions: Vec<JoinCondition>,
    left: LogicalOperator,
    right: LogicalOperator,
) -> LogicalOperator {
    let mut join = LogicalComparisonJoin::new(join_type, LogicalOperatorType::LogicalComparisonJoin);
    join.conditions = conditions;
    LogicalOperator::new(LogicalKind::ComparisonJoin(Box::new(join))).with_children(vec![left, right])
}

/// `row_number() OVER (PARTITION BY partitions ORDER BY orders ROWS UNBOUNDED PRECEDING)`.
fn row_number(partitions: Vec<Expression>, orders: Vec<BoundOrderByNode>, alias: &str) -> Expression {
    Expression::new(
        ExpressionType::WindowRowNumber,
        LogicalType::BIGINT,
        ExpressionKind::Window(Box::new(BoundWindowExpression {
            aggregate: None,
            bind_info: None,
            children: Vec::new(),
            partitions,
            orders,
            filter_expr: None,
            ignore_nulls: false,
            distinct: false,
            start: WindowBoundary::UnboundedPreceding,
            end: WindowBoundary::CurrentRowRows,
            exclude_clause: WindowExcludeMode::NoOther,
            start_expr: None,
            end_expr: None,
            offset_expr: None,
            default_expr: None,
            arg_orders: Vec::new(),
        })),
    )
    .with_alias(alias)
}

/// DuckDB's `SubqueryDependentFilter`: an (AND of) subquery predicate(s).
fn subquery_dependent_filter(expression: &Expression) -> bool {
    match &expression.kind {
        ExpressionKind::Conjunction(conjunction)
            if expression.expression_type == ExpressionType::ConjunctionAnd =>
        {
            conjunction.children.iter().any(subquery_dependent_filter)
        }
        ExpressionKind::Subquery(_) => true,
        _ => false,
    }
}

/// Takes child `index` out of `op`, leaving a placeholder.
fn take_child(op: &mut LogicalOperator, index: usize) -> Result<LogicalOperator> {
    let child = op
        .children
        .get_mut(index)
        .ok_or_else(|| Error::internal("operator lacks a child"))?;
    Ok(std::mem::replace(
        child,
        LogicalOperator::new(LogicalKind::DummyScan { table_index: 0 }),
    ))
}

/// The delim join conditions on the correlated columns (DuckDB's
/// `CreateDelimJoinConditions`).
fn create_delim_join_conditions(
    join: &mut LogicalComparisonJoin,
    correlated_columns: &CorrelatedColumns,
    bindings: &[ColumnBinding],
    base_offset: u64,
    perform_delim: bool,
) -> Result<()> {
    let mut start = 0;
    let mut end = if perform_delim {
        correlated_columns.len()
    } else {
        1
    };
    let delim_index = usize::try_from(correlated_columns.delim_index()).unwrap_or(usize::MAX);
    if !perform_delim && delim_index < correlated_columns.len() {
        start = delim_index;
        end = start + 1;
    }
    for i in start..end {
        let column = correlated_columns
            .get(i)
            .ok_or_else(|| Error::internal("Delim join - correlated column out of range"))?;
        let binding_index = usize::try_from(base_offset)
            .unwrap_or(usize::MAX)
            .saturating_add(i);
        let right = *bindings
            .get(binding_index)
            .ok_or_else(|| Error::internal("Delim join - binding index out of range"))?;
        join.conditions.push(JoinCondition {
            left: column_ref(&column.name, &column.logical_type, column.binding),
            right: column_ref(&column.name, &column.logical_type, right),
            comparison: ExpressionType::CompareNotDistinctFrom,
        });
    }
    Ok(())
}

/// The `first` aggregate of a correlated column (DuckDB's `FirstFunctionGetter`).
fn first_aggregate(binder: &Binder, child: Expression) -> Result<Expression> {
    let context = &binder.context;
    let entry = context
        .get_entry("system", "main", CatalogType::AggregateFunction, "first", true)?
        .ok_or_else(|| Error::not_implemented("aggregate function first"))?;
    let EntryKind::AggregateFunction(functions) = &entry.entry.kind else {
        return Err(Error::internal("first is not an aggregate function"));
    };
    let function_binder = FunctionBinder::new(context);
    let function =
        function_binder.bind_aggregate_overload(functions, std::slice::from_ref(&child.return_type))??;
    function_binder.bind_aggregate_function(function, vec![child], None, AggregateType::NonDistinct, None)
}

impl<'a> FlattenDependentJoins<'a> {
    fn new(
        binder: &'a Binder,
        correlated: &CorrelatedColumns,
        perform_delim: bool,
        any_join: bool,
        has_parent: bool,
    ) -> Self {
        let mut correlated_map = CorrelatedMap::new();
        let mut delim_types = Vec::with_capacity(correlated.len());
        for (i, column) in correlated.iter().enumerate() {
            correlated_map.insert(column.binding, i as u64);
            delim_types.push(column.logical_type.clone());
        }
        Self {
            binder,
            has_correlated_expressions: HashMap::new(),
            correlated_map,
            base_binding: ColumnBinding::default(),
            delim_offset: u64::MAX,
            data_offset: 0,
            correlated_columns: correlated.clone(),
            delim_types,
            perform_delim,
            any_join,
            has_parent,
            replacement_map: HashMap::new(),
            ctes: HashMap::new(),
        }
    }

    fn correlation(&self, op: &LogicalOperator) -> Result<bool> {
        self.has_correlated_expressions
            .get(&op.node_id())
            .copied()
            .ok_or_else(|| Error::internal("operator missing from the correlation analysis"))
    }

    /// Flattens the dependent joins in `plan` (DuckDB's `Decorrelate`).
    fn decorrelate(
        &mut self,
        mut plan: LogicalOperator,
        parent_propagate_null_values: bool,
        lateral_depth: u64,
    ) -> Result<LogicalOperator> {
        if !matches!(plan.kind, LogicalKind::DependentJoin(_)) {
            for index in 0..plan.children.len() {
                let child = take_child(&mut plan, index)?;
                plan.children[index] = self.decorrelate(child, true, 0)?;
            }
            return Ok(plan);
        }
        let node_id = plan.node_id();
        if self.has_parent {
            // Unnest the left side in the enclosing pass's context.
            let correlated = self.correlation(&plan)?;
            let left = take_child(&mut plan, 0)?;
            plan.children[0] = if correlated {
                self.push_down_dependent_join(left, parent_propagate_null_values, lateral_depth)?
            } else {
                self.decorrelate_independent(left)?
            };
            self.has_correlated_expressions.insert(node_id, false);
            rewrite_correlated_expressions(&mut plan, self.base_binding, &self.correlated_map, 0, false);
            rewrite_correlated_expressions(&mut plan, self.base_binding, &self.correlated_map, 0, true);
        } else {
            let left = take_child(&mut plan, 0)?;
            plan.children[0] = self.decorrelate(left, true, 0)?;
        }
        let LogicalKind::DependentJoin(join) = &plan.kind else {
            return Err(Error::internal("expected a dependent join"));
        };
        let join = (**join).clone();
        if !join.perform_delim {
            // Duplicate elimination happens on a row number of the left side.
            let delim_column = join
                .correlated_columns
                .get(usize::try_from(join.correlated_columns.delim_index()).unwrap_or(usize::MAX))
                .ok_or_else(|| Error::internal("missing delim index column"))?;
            let left = take_child(&mut plan, 0)?;
            plan.children[0] = LogicalOperator::new(LogicalKind::Window {
                window_index: delim_column.binding.table_index,
            })
            .with_expressions(vec![row_number(Vec::new(), Vec::new(), "delim_index")])
            .with_children(vec![left]);
        }
        let lateral_depth = 0;
        let propagate_null_values = join.propagate_null_values;
        let mut flatten = FlattenDependentJoins::new(
            self.binder,
            &join.correlated_columns,
            join.perform_delim,
            join.any_join,
            true,
        );
        flatten.ctes = std::mem::take(&mut self.ctes);
        let result = self.flatten_right_side(
            &mut plan,
            &mut flatten,
            &join,
            parent_propagate_null_values,
            propagate_null_values,
            lateral_depth,
        );
        self.ctes = std::mem::take(&mut flatten.ctes);
        result
    }

    /// The part of `Decorrelate` that pushes the dependent join into its right side.
    fn flatten_right_side(
        &mut self,
        plan: &mut LogicalOperator,
        flatten: &mut FlattenDependentJoins<'_>,
        join: &LogicalDependentJoin,
        parent_propagate_null_values: bool,
        propagate_null_values: bool,
        lateral_depth: u64,
    ) -> Result<LogicalOperator> {
        {
            let right = plan
                .children
                .get_mut(1)
                .ok_or_else(|| Error::internal("dependent join without a right side"))?;
            flatten.detect_correlated_expressions(right, join.is_lateral_join, lateral_depth, false)?;
        }
        if let LogicalKind::MaterializedCte { .. } = &plan.children[1].kind
            && let Some(left_of_cte) = plan.children[1].children.first()
            && flatten.has_correlated_expressions.get(&left_of_cte.node_id()) == Some(&false)
        {
            // The CTE definition is uncorrelated: push the dependent join below it.
            let mut cte = take_child(plan, 1)?;
            plan.children[1] = take_child(&mut cte, 1)?;
            let dependent = std::mem::replace(
                plan,
                LogicalOperator::new(LogicalKind::DummyScan { table_index: 0 }),
            );
            cte.children[1] = self.decorrelate(dependent, parent_propagate_null_values, lateral_depth)?;
            return Ok(cte);
        }
        let right = take_child(plan, 1)?;
        plan.children[1] = flatten.push_down_dependent_join(right, propagate_null_values, lateral_depth)?;
        self.data_offset = flatten.data_offset;
        let left_offset = plan.children[0].column_bindings().len() as u64;
        if !self.has_parent {
            self.delim_offset = left_offset + flatten.delim_offset;
        }
        rewrite_correlated_expressions(
            plan,
            self.base_binding,
            &self.correlated_map,
            lateral_depth,
            false,
        );

        let LogicalKind::DependentJoin(dependent) = &mut plan.kind else {
            return Err(Error::internal("expected a dependent join"));
        };
        dependent.join.duplicate_eliminated_columns.clear();
        dependent.join.mark_types.clear();
        for column in &dependent.correlated_columns {
            dependent.join.duplicate_eliminated_columns.push(column_ref(
                "",
                &column.logical_type,
                column.binding,
            ));
            dependent.join.mark_types.push(column.logical_type.clone());
        }
        let mut plan_columns = plan.children[1].column_bindings();
        let LogicalKind::DependentJoin(dependent) =
            std::mem::replace(&mut plan.kind, LogicalKind::CrossProduct)
        else {
            return Err(Error::internal("expected a dependent join"));
        };
        let dependent = *dependent;
        // The dependent join is now fully decorrelated: it becomes a DELIM join.
        let mut delim_join = dependent.join;
        delim_join.operator_type = LogicalOperatorType::LogicalDelimJoin;
        if dependent.is_lateral_join && dependent.subquery_type == SubqueryType::Invalid {
            if matches!(plan.children[1].kind, LogicalKind::MaterializedCte { .. })
                && let Some(output) = plan.children[1].children.get(1)
            {
                plan_columns = output.column_bindings();
            }
            create_delim_join_conditions(
                &mut delim_join,
                &dependent.correlated_columns,
                &plan_columns,
                flatten.delim_offset,
                dependent.perform_delim,
            )?;
            let join_type = delim_join.join_type;
            plan.kind = LogicalKind::ComparisonJoin(Box::new(delim_join));
            if !dependent.arbitrary_expressions.is_empty() {
                if join_type != JoinType::Inner {
                    return Err(Error::binder(
                        "Join condition for non-inner LATERAL JOIN must be a comparison between the left and right \
                         side",
                    ));
                }
                let input = std::mem::replace(
                    plan,
                    LogicalOperator::new(LogicalKind::DummyScan { table_index: 0 }),
                );
                return Ok(LogicalOperator::new(LogicalKind::Filter {
                    projection_map: Vec::new(),
                })
                .with_expressions(dependent.arbitrary_expressions)
                .with_children(vec![input]));
            }
            return Ok(std::mem::replace(
                plan,
                LogicalOperator::new(LogicalKind::DummyScan { table_index: 0 }),
            ));
        }
        create_delim_join_conditions(
            &mut delim_join,
            &dependent.correlated_columns,
            &plan_columns,
            flatten.delim_offset,
            dependent.perform_delim,
        )?;
        if dependent.subquery_type == SubqueryType::Any {
            // The ANY/ALL comparison itself.
            let context = self.binder.context.clone();
            for (i, left) in dependent.expression_children.into_iter().enumerate() {
                let child_type = &dependent.child_types[i];
                let compare_type = &dependent.child_targets[i];
                let column = column_ref("", child_type, plan_columns[i]);
                let right = add_cast_to_type(&context, column, compare_type, false)?;
                delim_join.conditions.push(JoinCondition {
                    left: push_collation(left, compare_type)?,
                    right: push_collation(right, compare_type)?,
                    comparison: dependent.comparison_type,
                });
            }
        }
        plan.kind = LogicalKind::ComparisonJoin(Box::new(delim_join));
        Ok(std::mem::replace(
            plan,
            LogicalOperator::new(LogicalKind::DummyScan { table_index: 0 }),
        ))
    }

    /// DuckDB's `DecorrelateIndependent`: a fresh pass without correlated columns.
    fn decorrelate_independent(&mut self, plan: LogicalOperator) -> Result<LogicalOperator> {
        let mut flatten =
            FlattenDependentJoins::new(self.binder, &CorrelatedColumns::default(), true, false, false);
        flatten.ctes = std::mem::take(&mut self.ctes);
        let result = flatten.decorrelate(plan, true, 0);
        self.ctes = std::mem::take(&mut flatten.ctes);
        result
    }

    /// Records which operators reference the correlated columns (DuckDB's
    /// `DetectCorrelatedExpressions`).
    fn detect_correlated_expressions(
        &mut self,
        op: &mut LogicalOperator,
        lateral: bool,
        lateral_depth: u64,
        parent_is_dependent_join: bool,
    ) -> Result<bool> {
        let is_lateral_join = matches!(op.kind, LogicalKind::DependentJoin(_));
        let mut has_correlation =
            has_correlated_expressions(op, &self.correlated_columns, lateral, lateral_depth)?;
        let node_id = op.node_id();
        let materialized_index = match &op.kind {
            LogicalKind::MaterializedCte { table_index, .. } => Some(*table_index),
            _ => None,
        };
        for (child_index, child) in op.children.iter_mut().enumerate() {
            let child_lateral_depth = if is_lateral_join && child_index == 1 {
                lateral_depth + 1
            } else {
                lateral_depth
            };
            let condition = (parent_is_dependent_join || is_lateral_join) && child_index == 0;
            if self.detect_correlated_expressions(child, lateral, child_lateral_depth, condition)? {
                has_correlation = true;
            }
            if let Some(table_index) = materialized_index
                && child_index == 0
            {
                self.ctes.insert(
                    table_index,
                    CteNode {
                        node_id,
                        recursive: false,
                        correlated_columns: if has_correlation {
                            self.correlated_columns.clone()
                        } else {
                            CorrelatedColumns::default()
                        },
                    },
                );
                self.has_correlated_expressions.insert(node_id, has_correlation);
            }
        }
        if has_correlation
            && let LogicalKind::MaterializedCte {
                correlated_columns, ..
            } = &mut op.kind
        {
            correlated_columns.clone_from(&self.correlated_columns);
        }
        if let LogicalKind::CteRef { cte_index, .. } = &op.kind
            && !self.has_correlated_expressions.contains_key(&node_id)
        {
            self.has_correlated_expressions.insert(node_id, false);
            return Ok(match self.ctes.get(cte_index).cloned() {
                Some(cte) if cte.recursive => true,
                Some(cte) => {
                    let correlated = self
                        .has_correlated_expressions
                        .get(&cte.node_id)
                        .copied()
                        .unwrap_or(false);
                    self.has_correlated_expressions.insert(node_id, correlated);
                    correlated
                }
                None => true,
            });
        }
        self.has_correlated_expressions.insert(node_id, has_correlation);
        if let LogicalKind::RecursiveCte { table_index, .. } = &op.kind {
            let table_index = *table_index;
            self.ctes.insert(
                table_index,
                CteNode {
                    node_id,
                    recursive: true,
                    correlated_columns: if has_correlation {
                        self.correlated_columns.clone()
                    } else {
                        CorrelatedColumns::default()
                    },
                },
            );
            if has_correlation {
                if let LogicalKind::RecursiveCte {
                    correlated_columns, ..
                } = &mut op.kind
                {
                    correlated_columns.clone_from(&self.correlated_columns);
                }
                if let Some(right) = op.children.get_mut(1) {
                    self.mark_subtree_correlated(right, table_index)?;
                }
            }
        }
        Ok(has_correlation)
    }

    /// DuckDB's `MarkSubtreeCorrelated`.
    fn mark_subtree_correlated(&mut self, op: &mut LogicalOperator, cte_index: u64) -> Result<bool> {
        let mut has_correlation = self.correlation(op)?;
        for child in &mut op.children {
            has_correlation |= self.mark_subtree_correlated(child, cte_index)?;
        }
        let node_id = op.node_id();
        if !matches!(op.kind, LogicalKind::Get(_)) || op.children.len() == 1 {
            if let LogicalKind::CteRef {
                cte_index: referenced,
                ..
            } = &op.kind
            {
                if *referenced != cte_index {
                    self.has_correlated_expressions.insert(node_id, has_correlation);
                    return Ok(has_correlation);
                }
                self.has_correlated_expressions.insert(node_id, true);
                return Ok(true);
            }
            self.has_correlated_expressions.insert(node_id, has_correlation);
        }
        Ok(has_correlation)
    }

    /// DuckDB's `PushDownDependentJoin`.
    fn push_down_dependent_join(
        &mut self,
        plan: LogicalOperator,
        propagate_null_values: bool,
        lateral_depth: u64,
    ) -> Result<LogicalOperator> {
        let mut propagate_null_values = propagate_null_values;
        let mut result = self.push_down_internal(plan, &mut propagate_null_values, lateral_depth)?;
        if !self.replacement_map.is_empty() {
            rewrite_count_aggregates(&mut result, &self.replacement_map)?;
        }
        Ok(result)
    }

    /// Adds the duplicate-eliminated scan to an uncorrelated subtree.
    fn push_into_uncorrelated(
        &mut self,
        mut plan: LogicalOperator,
        lateral_depth: u64,
    ) -> Result<(Option<LogicalOperator>, LogicalOperator)> {
        if let LogicalKind::CteRef {
            cte_index,
            correlated_columns,
            ..
        } = &plan.kind
            && let Some(cte) = self.ctes.get(cte_index)
            && *correlated_columns == 0
        {
            let (cte_index, columns) = (*cte_index, cte.correlated_columns.clone());
            rewrite_cte_scan(
                &mut plan,
                cte_index,
                &columns,
                CteScanRewriteMode::WithRecursiveDependentJoins,
            );
        }
        let _ = lateral_depth;
        let delim_index = self.binder.generate_table_index();
        self.base_binding = ColumnBinding::new(delim_index, 0);
        self.delim_offset = plan.column_bindings().len() as u64;
        self.data_offset = 0;
        let delim_scan = delim_get(delim_index, &self.delim_types);
        match &plan.kind {
            // Keep the projection on top, for the positions of its columns.
            LogicalKind::Projection { .. } => Ok((Some(delim_scan), plan)),
            LogicalKind::CteRef {
                table_index,
                chunk_types,
                correlated_columns,
                ..
            } => {
                // A reference to a correlated CTE: join on the correlated columns.
                let left = ColumnBinding::new(
                    *table_index,
                    (chunk_types.len() as u64).saturating_sub(*correlated_columns),
                );
                let mut conditions = Vec::new();
                for i in 0..*correlated_columns {
                    let column = self
                        .correlated_columns
                        .get(usize::try_from(i).unwrap_or(usize::MAX))
                        .ok_or_else(|| Error::internal("CTE correlated column out of range"))?;
                    conditions.push(JoinCondition {
                        left: column_ref(
                            "",
                            &column.logical_type,
                            ColumnBinding::new(left.table_index, left.column_index + i),
                        ),
                        right: column_ref(
                            "",
                            &column.logical_type,
                            ColumnBinding::new(
                                self.base_binding.table_index,
                                self.base_binding.column_index + i,
                            ),
                        ),
                        comparison: ExpressionType::CompareNotDistinctFrom,
                    });
                }
                Ok((
                    None,
                    comparison_join(JoinType::Inner, conditions, plan, delim_scan),
                ))
            }
            _ => {
                let decorrelated = self.decorrelate(plan, true, 0)?;
                Ok((None, LogicalOperator::cross_product(decorrelated, delim_scan)))
            }
        }
    }

    /// Re-points correlated references of `plan`'s own expressions at the delim scan.
    fn rewrite(&self, plan: &mut LogicalOperator, lateral_depth: u64) {
        rewrite_correlated_expressions(
            plan,
            self.base_binding,
            &self.correlated_map,
            lateral_depth,
            false,
        );
    }

    /// Correlated column `i` as a reference to the current base binding.
    fn base_column(&self, i: usize, with_name: bool) -> Result<Expression> {
        let column = self
            .correlated_columns
            .get(i)
            .ok_or_else(|| Error::internal("correlated column out of range"))?;
        Ok(column_ref(
            if with_name { &column.name } else { "" },
            &column.logical_type,
            ColumnBinding::new(
                self.base_binding.table_index,
                self.base_binding.column_index + i as u64,
            ),
        ))
    }

    /// DuckDB's `PushDownDependentJoinInternal`.
    #[allow(clippy::too_many_lines)]
    fn push_down_internal(
        &mut self,
        mut plan: LogicalOperator,
        parent_propagate_null_values: &mut bool,
        lateral_depth: u64,
    ) -> Result<LogicalOperator> {
        let mut exit_projection_scan = None;
        if !self.correlation(&plan)? {
            // No correlated expressions below: a cross product with the delim scan.
            let (delim_scan, result) = self.push_into_uncorrelated(plan, lateral_depth)?;
            match delim_scan {
                None => return Ok(result),
                Some(scan) => {
                    exit_projection_scan = Some(scan);
                    plan = result;
                }
            }
        }
        let correlated_count = self.correlated_columns.len();
        match plan.operator_type() {
            LogicalOperatorType::LogicalUnnest | LogicalOperatorType::LogicalFilter => {
                for expression in &plan.expressions {
                    self.any_join |= subquery_dependent_filter(expression);
                }
                let child = take_child(&mut plan, 0)?;
                plan.children[0] =
                    self.push_down_internal(child, parent_propagate_null_values, lateral_depth)?;
                self.rewrite(&mut plan, lateral_depth);
                Ok(plan)
            }
            LogicalOperatorType::LogicalProjection => {
                for expression in &plan.expressions {
                    *parent_propagate_null_values &= expression.propagates_null_values();
                }
                let child_is_dependent_join = matches!(plan.children[0].kind, LogicalKind::DependentJoin(_));
                *parent_propagate_null_values &= !child_is_dependent_join;
                let child = take_child(&mut plan, 0)?;
                plan.children[0] = match exit_projection_scan {
                    Some(scan) => LogicalOperator::cross_product(self.decorrelate(child, true, 0)?, scan),
                    None => self.push_down_internal(child, parent_propagate_null_values, lateral_depth)?,
                };
                self.rewrite(&mut plan, lateral_depth);
                for i in 0..correlated_count {
                    let column = self.base_column(i, true)?;
                    plan.expressions.push(column);
                }
                let LogicalKind::Projection { table_index } = plan.kind else {
                    return Err(Error::internal("expected a projection"));
                };
                self.base_binding =
                    ColumnBinding::new(table_index, (plan.expressions.len() - correlated_count) as u64);
                self.delim_offset = self.base_binding.column_index;
                self.data_offset = 0;
                Ok(plan)
            }
            LogicalOperatorType::LogicalAggregateAndGroupBy => {
                self.push_into_aggregate(plan, parent_propagate_null_values, lateral_depth)
            }
            LogicalOperatorType::LogicalCrossProduct => {
                let left_correlated = self.correlation(&plan.children[0])?;
                let right_correlated = self.correlation(&plan.children[1])?;
                if !right_correlated {
                    let left = take_child(&mut plan, 0)?;
                    plan.children[0] =
                        self.push_down_internal(left, parent_propagate_null_values, lateral_depth)?;
                    let right = take_child(&mut plan, 1)?;
                    plan.children[1] = self.decorrelate_independent(right)?;
                    return Ok(plan);
                }
                if !left_correlated {
                    let right = take_child(&mut plan, 1)?;
                    plan.children[1] =
                        self.push_down_internal(right, parent_propagate_null_values, lateral_depth)?;
                    let left = take_child(&mut plan, 0)?;
                    plan.children[0] = self.decorrelate_independent(left)?;
                    self.delim_offset += plan.children[0].column_bindings().len() as u64;
                    return Ok(plan);
                }
                // Both sides are correlated: an inner join on the correlated columns.
                let right = take_child(&mut plan, 1)?;
                let right = self.push_down_internal(right, parent_propagate_null_values, lateral_depth)?;
                let right_binding = self.base_binding;
                let left = take_child(&mut plan, 0)?;
                let left = self.push_down_internal(left, parent_propagate_null_values, lateral_depth)?;
                let mut conditions = Vec::with_capacity(correlated_count);
                for i in 0..correlated_count {
                    let ty = self
                        .correlated_columns
                        .get(i)
                        .map(|c| c.logical_type.clone())
                        .unwrap_or_default();
                    conditions.push(JoinCondition {
                        left: column_ref(
                            "",
                            &ty,
                            ColumnBinding::new(
                                self.base_binding.table_index,
                                self.base_binding.column_index + i as u64,
                            ),
                        ),
                        right: column_ref(
                            "",
                            &ty,
                            ColumnBinding::new(
                                right_binding.table_index,
                                right_binding.column_index + i as u64,
                            ),
                        ),
                        comparison: ExpressionType::CompareNotDistinctFrom,
                    });
                }
                Ok(comparison_join(JoinType::Inner, conditions, left, right))
            }
            LogicalOperatorType::LogicalDependentJoin => {
                self.decorrelate(plan, *parent_propagate_null_values, lateral_depth)
            }
            LogicalOperatorType::LogicalAnyJoin
            | LogicalOperatorType::LogicalAsofJoin
            | LogicalOperatorType::LogicalComparisonJoin => {
                self.push_into_join(plan, parent_propagate_null_values, lateral_depth)
            }
            LogicalOperatorType::LogicalLimit => {
                self.push_into_limit(plan, parent_propagate_null_values, lateral_depth)
            }
            LogicalOperatorType::LogicalWindow => {
                let child = take_child(&mut plan, 0)?;
                plan.children[0] =
                    self.push_down_internal(child, parent_propagate_null_values, lateral_depth)?;
                self.rewrite(&mut plan, lateral_depth);
                let mut partitions = Vec::with_capacity(correlated_count);
                for i in 0..correlated_count {
                    partitions.push(self.base_column(i, false)?);
                }
                for expression in &mut plan.expressions {
                    if let ExpressionKind::Window(window) = &mut expression.kind {
                        window.partitions.extend(partitions.iter().cloned());
                    }
                }
                Ok(plan)
            }
            LogicalOperatorType::LogicalExcept
            | LogicalOperatorType::LogicalIntersect
            | LogicalOperatorType::LogicalUnion => {
                for index in 0..plan.children.len() {
                    let child = take_child(&mut plan, index)?;
                    let mut child = self.push_down_dependent_join(child, true, 0)?;
                    if matches!(child.kind, LogicalKind::CrossProduct) {
                        // Fix the column order with a projection.
                        child.resolve_operator_types()?;
                        let projection_index = self.binder.generate_table_index();
                        let expressions = child
                            .column_bindings()
                            .into_iter()
                            .zip(child.types.clone())
                            .map(|(binding, ty)| column_ref("", &ty, binding))
                            .collect();
                        child = LogicalOperator::projection(projection_index, expressions, child);
                    }
                    plan.children[index] = child;
                }
                let LogicalKind::SetOperation {
                    table_index,
                    column_count,
                    ..
                } = &mut plan.kind
                else {
                    return Err(Error::internal("expected a set operation"));
                };
                self.base_binding = ColumnBinding::new(*table_index, *column_count);
                *column_count += correlated_count as u64;
                Ok(plan)
            }
            LogicalOperatorType::LogicalDistinct => {
                let child = take_child(&mut plan, 0)?;
                plan.children[0] = self.push_down_dependent_join(child, true, 0)?;
                let mut targets = Vec::with_capacity(correlated_count);
                for i in 0..correlated_count {
                    targets.push(self.base_column(i, false)?);
                }
                if let LogicalKind::Distinct { distinct_targets, .. } = &mut plan.kind {
                    distinct_targets.extend(targets);
                }
                Ok(plan)
            }
            LogicalOperatorType::LogicalExpressionGet => {
                let child = take_child(&mut plan, 0)?;
                plan.children[0] =
                    self.push_down_internal(child, parent_propagate_null_values, lateral_depth)?;
                self.rewrite(&mut plan, lateral_depth);
                let mut columns = Vec::with_capacity(correlated_count);
                for i in 0..correlated_count {
                    columns.push(self.base_column(i, false)?);
                }
                let LogicalKind::ExpressionGet {
                    table_index,
                    expr_types,
                    expressions,
                } = &mut plan.kind
                else {
                    return Err(Error::internal("expected an expression get"));
                };
                for column in columns {
                    for row in expressions.iter_mut() {
                        row.push(column.clone());
                    }
                    expr_types.push(column.return_type.clone());
                }
                self.base_binding =
                    ColumnBinding::new(*table_index, (expr_types.len() - correlated_count) as u64);
                self.delim_offset = self.base_binding.column_index;
                self.data_offset = 0;
                Ok(plan)
            }
            LogicalOperatorType::LogicalPivot => Err(Error::binder(
                "PIVOT is not supported in correlated subqueries yet",
            )),
            LogicalOperatorType::LogicalOrderBy => {
                let child = take_child(&mut plan, 0)?;
                plan.children[0] = self.push_down_dependent_join(child, true, 0)?;
                Ok(plan)
            }
            LogicalOperatorType::LogicalGet => {
                if plan.children.len() != 1 {
                    return Err(Error::internal(
                        "Flatten dependent joins - logical get encountered without children",
                    ));
                }
                let child = take_child(&mut plan, 0)?;
                plan.children[0] = self.push_down_dependent_join(child, true, 0)?;
                let delim_offset = self.delim_offset;
                if let LogicalKind::Get(get) = &mut plan.kind {
                    for i in 0..correlated_count {
                        get.projected_input.push(delim_offset + i as u64);
                    }
                    self.delim_offset = get.column_ids.len() as u64;
                }
                self.data_offset = 0;
                self.rewrite(&mut plan, lateral_depth);
                Ok(plan)
            }
            LogicalOperatorType::LogicalMaterializedCte | LogicalOperatorType::LogicalRecursiveCte => {
                self.push_into_cte(plan, parent_propagate_null_values, lateral_depth)
            }
            LogicalOperatorType::LogicalCteRef => {
                let LogicalKind::CteRef {
                    table_index,
                    chunk_types,
                    correlated_columns,
                    ..
                } = &plan.kind
                else {
                    return Err(Error::internal("expected a CTE reference"));
                };
                self.base_binding = ColumnBinding::new(
                    *table_index,
                    (chunk_types.len() as u64).saturating_sub(*correlated_columns),
                );
                Ok(plan)
            }
            LogicalOperatorType::LogicalDelimJoin => Err(Error::binder(
                "Nested lateral joins or lateral joins in correlated subqueries are not (yet) supported",
            )),
            LogicalOperatorType::LogicalSample => Err(Error::binder(
                "Sampling in correlated subqueries is not (yet) supported",
            )),
            LogicalOperatorType::LogicalPositionalJoin => Err(Error::binder(
                "Positional join in correlated subqueries is not (yet) supported",
            )),
            other => Err(Error::internal(format!(
                "Logical operator type \"{}\" for dependent join",
                other.name()
            ))),
        }
    }

    /// The aggregate case of `PushDownDependentJoinInternal`: the correlated columns
    /// become groups; ungrouped aggregates are joined with the delim scan.
    fn push_into_aggregate(
        &mut self,
        mut plan: LogicalOperator,
        parent_propagate_null_values: &mut bool,
        lateral_depth: u64,
    ) -> Result<LogicalOperator> {
        for expression in &plan.expressions {
            *parent_propagate_null_values &= expression.propagates_null_values();
        }
        let child = take_child(&mut plan, 0)?;
        plan.children[0] = self.push_down_internal(child, parent_propagate_null_values, lateral_depth)?;
        self.rewrite(&mut plan, lateral_depth);
        let correlated_count = self.correlated_columns.len();
        let new_group_count = if self.perform_delim { correlated_count } else { 1 };
        let mut new_groups = Vec::with_capacity(new_group_count);
        for i in 0..new_group_count {
            new_groups.push(self.base_column(i, true)?);
        }
        let mut firsts = Vec::new();
        if !self.perform_delim {
            for i in 0..correlated_count {
                let column = self.base_column(i, true)?;
                firsts.push(first_aggregate(self.binder, column)?);
            }
        }
        let LogicalKind::Aggregate(aggregate) = &mut plan.kind else {
            return Err(Error::internal("expected an aggregate"));
        };
        for group in new_groups {
            for set in &mut aggregate.grouping_sets {
                set.insert(aggregate.groups.len() as u64);
            }
            aggregate.groups.push(group);
        }
        let (delim_table_index, delim_column_offset, delim_data_offset) = if self.perform_delim {
            (
                aggregate.group_index,
                (aggregate.groups.len() - correlated_count) as u64,
                aggregate.groups.len() as u64,
            )
        } else {
            let offset = plan.expressions.len() as u64;
            plan.expressions.extend(firsts);
            (aggregate.aggregate_index, offset, aggregate.groups.len() as u64)
        };
        let ungrouped_join = if aggregate.grouping_sets.is_empty() {
            aggregate.groups.len() == new_group_count
        } else {
            aggregate
                .grouping_sets
                .iter()
                .any(|set| set.len() == new_group_count)
        };
        if !ungrouped_join {
            self.base_binding = ColumnBinding::new(delim_table_index, delim_column_offset);
            self.delim_offset = delim_column_offset;
            self.data_offset = delim_data_offset;
            return Ok(plan);
        }
        // An INNER or LEFT join between the delim scan and the aggregate's result.
        let mut join_type = if self.any_join || !*parent_propagate_null_values {
            JoinType::Left
        } else {
            JoinType::Inner
        };
        if plan
            .expressions
            .iter()
            .any(|expression| !expression.propagates_null_values())
        {
            join_type = JoinType::Left;
        }
        let aggregate_index = aggregate.aggregate_index;
        let left_index = self.binder.generate_table_index();
        let mut conditions = Vec::with_capacity(new_group_count);
        for i in 0..new_group_count {
            let column = self
                .correlated_columns
                .get(i)
                .ok_or_else(|| Error::internal("correlated column out of range"))?;
            conditions.push(JoinCondition {
                left: column_ref(
                    &column.name,
                    &column.logical_type,
                    ColumnBinding::new(left_index, i as u64),
                ),
                right: column_ref(
                    "",
                    &column.logical_type,
                    ColumnBinding::new(delim_table_index, delim_column_offset + i as u64),
                ),
                comparison: ExpressionType::CompareNotDistinctFrom,
            });
        }
        // References to COUNT become `CASE WHEN count IS NULL THEN 0 ELSE count END`.
        for (i, expression) in plan.expressions.iter().enumerate() {
            if let ExpressionKind::Aggregate(aggregate) = &expression.kind
                && matches!(&*aggregate.function.name, "count" | "count_star")
            {
                self.replacement_map
                    .insert(ColumnBinding::new(aggregate_index, i as u64), i as u64);
            }
        }
        self.base_binding = ColumnBinding::new(left_index, 0);
        self.delim_offset = 0;
        self.data_offset = 0;
        Ok(comparison_join(
            join_type,
            conditions,
            delim_get(left_index, &self.delim_types),
            plan,
        ))
    }

    /// The join case of `PushDownDependentJoinInternal`.
    fn push_into_join(
        &mut self,
        mut plan: LogicalOperator,
        parent_propagate_null_values: &mut bool,
        lateral_depth: u64,
    ) -> Result<LogicalOperator> {
        let join_type = plan
            .join_type()
            .ok_or_else(|| Error::internal("join without a join type"))?;
        let left_correlated = self.correlation(&plan.children[0])?;
        let right_correlated = self.correlation(&plan.children[1])?;
        let correlated_count = self.correlated_columns.len();
        match join_type {
            JoinType::Inner => {
                if !right_correlated {
                    let left = take_child(&mut plan, 0)?;
                    plan.children[0] =
                        self.push_down_internal(left, parent_propagate_null_values, lateral_depth)?;
                    let right = take_child(&mut plan, 1)?;
                    plan.children[1] = self.decorrelate_independent(right)?;
                    return Ok(plan);
                }
                if !left_correlated {
                    let right = take_child(&mut plan, 1)?;
                    plan.children[1] =
                        self.push_down_internal(right, parent_propagate_null_values, lateral_depth)?;
                    let left = take_child(&mut plan, 0)?;
                    plan.children[0] = self.decorrelate_independent(left)?;
                    self.delim_offset += plan.children[0].column_bindings().len() as u64;
                    return Ok(plan);
                }
            }
            JoinType::Left => {
                if !right_correlated {
                    let left = take_child(&mut plan, 0)?;
                    plan.children[0] =
                        self.push_down_internal(left, parent_propagate_null_values, lateral_depth)?;
                    let right = take_child(&mut plan, 1)?;
                    plan.children[1] = self.decorrelate_independent(right)?;
                    return Ok(plan);
                }
            }
            JoinType::Right => {
                if !left_correlated {
                    let right = take_child(&mut plan, 1)?;
                    plan.children[1] =
                        self.push_down_internal(right, parent_propagate_null_values, lateral_depth)?;
                    let left = take_child(&mut plan, 0)?;
                    plan.children[0] = self.decorrelate_independent(left)?;
                    self.delim_offset += plan.children[0].column_bindings().len() as u64;
                    return Ok(plan);
                }
            }
            JoinType::Mark => {
                if !left_correlated && right_correlated {
                    // Push into the right side, then into the left one too, so that the
                    // correlated columns are available to the join condition.
                    let right = take_child(&mut plan, 1)?;
                    plan.children[1] =
                        self.push_down_internal(right, parent_propagate_null_values, lateral_depth)?;
                    let right_binding = self.base_binding;
                    let left = take_child(&mut plan, 0)?;
                    plan.children[0] =
                        self.push_down_internal(left, parent_propagate_null_values, lateral_depth)?;
                    let left_binding = self.base_binding;
                    let mut conditions = Vec::with_capacity(correlated_count);
                    for i in 0..correlated_count {
                        let ty = self
                            .correlated_columns
                            .get(i)
                            .map(|c| c.logical_type.clone())
                            .unwrap_or_default();
                        conditions.push(JoinCondition {
                            left: column_ref(
                                "",
                                &ty,
                                ColumnBinding::new(
                                    left_binding.table_index,
                                    left_binding.column_index + i as u64,
                                ),
                            ),
                            right: column_ref(
                                "",
                                &ty,
                                ColumnBinding::new(
                                    right_binding.table_index,
                                    right_binding.column_index + i as u64,
                                ),
                            ),
                            comparison: ExpressionType::CompareNotDistinctFrom,
                        });
                    }
                    if let LogicalKind::ComparisonJoin(join) = &mut plan.kind {
                        join.conditions.extend(conditions);
                    }
                    return Ok(plan);
                }
                let left = take_child(&mut plan, 0)?;
                plan.children[0] =
                    self.push_down_internal(left, parent_propagate_null_values, lateral_depth)?;
                let right = take_child(&mut plan, 1)?;
                plan.children[1] = self.decorrelate_independent(right)?;
                self.rewrite(&mut plan, lateral_depth);
                return Ok(plan);
            }
            _ => {
                return Err(Error::not_implemented(
                    "Unsupported join type for flattening correlated subquery",
                ));
            }
        }
        // Both sides are correlated: push into both.
        let left = take_child(&mut plan, 0)?;
        plan.children[0] = self.push_down_internal(left, parent_propagate_null_values, lateral_depth)?;
        let left_delim_offset = self.delim_offset;
        let left_binding = self.base_binding;
        let right = take_child(&mut plan, 1)?;
        plan.children[1] = self.push_down_internal(right, parent_propagate_null_values, lateral_depth)?;
        let right_binding = self.base_binding;
        // After an outer join, the base binding is the side that keeps its values.
        if join_type == JoinType::Left {
            self.base_binding = left_binding;
            self.delim_offset = left_delim_offset;
        } else if join_type == JoinType::Right {
            self.base_binding = right_binding;
            self.delim_offset += plan.children[0].column_bindings().len() as u64;
        }
        for i in 0..correlated_count {
            let ty = self
                .correlated_columns
                .get(i)
                .map(|c| c.logical_type.clone())
                .unwrap_or_default();
            let left = column_ref(
                "",
                &ty,
                ColumnBinding::new(left_binding.table_index, left_binding.column_index + i as u64),
            );
            let right = column_ref(
                "",
                &ty,
                ColumnBinding::new(right_binding.table_index, right_binding.column_index + i as u64),
            );
            match &mut plan.kind {
                LogicalKind::ComparisonJoin(join) => join.conditions.push(JoinCondition {
                    left,
                    right,
                    comparison: ExpressionType::CompareNotDistinctFrom,
                }),
                LogicalKind::AnyJoin { condition, .. } => {
                    let comparison =
                        Expression::comparison(ExpressionType::CompareNotDistinctFrom, left, right);
                    let previous =
                        std::mem::replace(&mut **condition, Expression::constant(Value::boolean(true)));
                    **condition =
                        Expression::conjunction(ExpressionType::ConjunctionAnd, vec![comparison, previous]);
                }
                _ => return Err(Error::internal("expected a join")),
            }
        }
        rewrite_correlated_expressions(
            &mut plan,
            right_binding,
            &self.correlated_map,
            lateral_depth,
            false,
        );
        Ok(plan)
    }

    /// The LIMIT case of `PushDownDependentJoinInternal`: a row number per correlated
    /// group, filtered to the limit.
    fn push_into_limit(
        &mut self,
        mut plan: LogicalOperator,
        parent_propagate_null_values: &mut bool,
        lateral_depth: u64,
    ) -> Result<LogicalOperator> {
        let LogicalKind::Limit { limit, offset } = &plan.kind else {
            return Err(Error::internal("expected a limit"));
        };
        match limit {
            BoundLimitNode::ConstantPercentage(_) | BoundLimitNode::ExpressionPercentage(_) => {
                return Err(Error::parser(
                    "Limit percent operator not supported in correlated subquery",
                ));
            }
            BoundLimitNode::ExpressionValue(_) => {
                return Err(Error::parser(
                    "Non-constant limit not supported in correlated subquery",
                ));
            }
            _ => {}
        }
        match offset {
            BoundLimitNode::ExpressionValue(_) => {
                return Err(Error::parser(
                    "Non-constant offset not supported in correlated subquery",
                ));
            }
            BoundLimitNode::ConstantPercentage(_) | BoundLimitNode::ExpressionPercentage(_) => {
                return Err(Error::internal("Percentage offset in FlattenDependentJoin"));
            }
            _ => {}
        }
        let (limit, offset) = (limit.clone(), offset.clone());
        let mut input = take_child(&mut plan, 0)?;
        // An ORDER BY directly below moves into the row number's ordering.
        let mut orders = Vec::new();
        let child = if let LogicalKind::Order { orders: order_by, .. } = &mut input.kind {
            orders = std::mem::take(order_by);
            let below = take_child(&mut input, 0)?;
            self.push_down_internal(below, parent_propagate_null_values, lateral_depth)?
        } else {
            self.push_down_internal(input, parent_propagate_null_values, lateral_depth)?
        };
        let child_column_count = child.column_bindings().len();
        let window_index = self.binder.generate_table_index();
        let partition_count = if self.perform_delim {
            self.correlated_columns.len()
        } else {
            1
        };
        let mut partitions = Vec::with_capacity(partition_count);
        for i in 0..partition_count {
            partitions.push(self.base_column(i, true)?);
        }
        let window = LogicalOperator::new(LogicalKind::Window { window_index })
            .with_expressions(vec![row_number(partitions, orders, "")])
            .with_children(vec![child]);
        let row_number_ref = || {
            Expression::column_ref(
                "limit_rownum",
                LogicalType::BIGINT,
                ColumnBinding::new(window_index, 0),
                0,
            )
        };
        let mut condition = None;
        if let BoundLimitNode::ConstantValue(limit) = limit {
            let limit = i64::try_from(limit).unwrap_or(i64::MAX);
            let upper = match offset {
                BoundLimitNode::ConstantValue(offset) => limit
                    .checked_add(i64::try_from(offset).unwrap_or(i64::MAX))
                    .unwrap_or(i64::MAX),
                _ => limit,
            };
            condition = Some(Expression::comparison(
                ExpressionType::CompareLessthanorequalto,
                row_number_ref(),
                Expression::constant(Value::bigint(upper)),
            ));
        }
        if let BoundLimitNode::ConstantValue(offset) = offset {
            let lower = Expression::comparison(
                ExpressionType::CompareGreaterthan,
                row_number_ref(),
                Expression::constant(Value::bigint(i64::try_from(offset).unwrap_or(i64::MAX))),
            );
            condition = Some(match condition {
                Some(upper) => Expression::conjunction(ExpressionType::ConjunctionAnd, vec![lower, upper]),
                None => lower,
            });
        }
        let condition =
            condition.ok_or_else(|| Error::internal("LIMIT without a constant limit or offset"))?;
        // The projection map prunes the row number.
        Ok(LogicalOperator::new(LogicalKind::Filter {
            projection_map: (0..child_column_count as u64).collect(),
        })
        .with_expressions(vec![condition])
        .with_children(vec![window]))
    }

    /// The CTE case of `PushDownDependentJoinInternal`.
    fn push_into_cte(
        &mut self,
        mut plan: LogicalOperator,
        parent_propagate_null_values: &mut bool,
        lateral_depth: u64,
    ) -> Result<LogicalOperator> {
        let is_recursive = matches!(plan.kind, LogicalKind::RecursiveCte { .. });
        if !is_recursive {
            let left_correlated = self.correlation(&plan.children[0])?;
            let right_correlated = self.correlation(&plan.children[1])?;
            if !left_correlated && right_correlated {
                let right = take_child(&mut plan, 1)?;
                plan.children[1] =
                    self.push_down_internal(right, parent_propagate_null_values, lateral_depth)?;
                let left = take_child(&mut plan, 0)?;
                plan.children[0] = self.decorrelate_independent(left)?;
                return Ok(plan);
            }
        }
        let left = take_child(&mut plan, 0)?;
        plan.children[0] = self.push_down_internal(left, parent_propagate_null_values, lateral_depth)?;
        let node_id = plan.node_id();
        let correlated = self.correlated_columns.clone();
        let (table_index, column_count) = match &mut plan.kind {
            LogicalKind::MaterializedCte {
                table_index,
                column_count,
                correlated_columns,
                ..
            }
            | LogicalKind::RecursiveCte {
                table_index,
                column_count,
                correlated_columns,
                ..
            } => {
                correlated_columns.clone_from(&correlated);
                (*table_index, *column_count)
            }
            _ => return Err(Error::internal("expected a CTE")),
        };
        self.base_binding = ColumnBinding::new(table_index, column_count);
        self.ctes.insert(
            table_index,
            CteNode {
                node_id,
                recursive: is_recursive,
                correlated_columns: correlated.clone(),
            },
        );
        if let LogicalKind::RecursiveCte { key_targets, .. } = &mut plan.kind
            && !key_targets.is_empty()
        {
            for (i, column) in correlated.iter().enumerate() {
                key_targets.push(column_ref(
                    "",
                    &column.logical_type,
                    ColumnBinding::new(
                        self.base_binding.table_index,
                        self.base_binding.column_index + i as u64,
                    ),
                ));
            }
        }
        let mode = if is_recursive {
            CteScanRewriteMode::WithRecursiveDependentJoins
        } else {
            CteScanRewriteMode::WithNonRecursiveDependentJoins
        };
        rewrite_cte_scan(&mut plan.children[1], table_index, &correlated, mode);
        *parent_propagate_null_values = false;
        let right = take_child(&mut plan, 1)?;
        plan.children[1] = self.push_down_internal(right, parent_propagate_null_values, lateral_depth)?;
        rewrite_correlated_expressions(
            &mut plan,
            self.base_binding,
            &self.correlated_map,
            lateral_depth,
            false,
        );
        for index in 0..2 {
            rewrite_correlated_expressions(
                &mut plan.children[index],
                self.base_binding,
                &self.correlated_map,
                lateral_depth + 1,
                true,
            );
        }
        if is_recursive {
            self.base_binding = ColumnBinding::new(table_index, column_count);
        }
        match &mut plan.kind {
            LogicalKind::MaterializedCte { column_count, .. }
            | LogicalKind::RecursiveCte { column_count, .. } => {
                *column_count += correlated.len() as u64;
            }
            _ => {}
        }
        Ok(plan)
    }
}

/// Flattens every dependent join of a plan (DuckDB's
/// `FlattenDependentJoins::DecorrelateIndependent`, run by the planner after binding).
pub fn decorrelate_independent(binder: &Binder, plan: LogicalOperator) -> Result<LogicalOperator> {
    FlattenDependentJoins::new(binder, &CorrelatedColumns::default(), true, false, false)
        .decorrelate(plan, true, 0)
}
