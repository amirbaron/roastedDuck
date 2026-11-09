//! Physical planning of ASOF joins (DuckDB's `plan_asof_join.cpp`): small left inputs and
//! joins with an extra predicate become a nested loop join whose matches are reduced to
//! the closest one per left row by an aggregate; everything else uses the as-of join.

use std::sync::Arc;

use roastedduck_ast::{ExpressionType, JoinType, WindowBoundary, WindowExcludeMode};
use roastedduck_common::{Error, Result};
use roastedduck_function::{
    AggregateFunction, AggregateFunctionSet, BindInput, FunctionClient, FunctionData,
};
use roastedduck_functions::aggregate::distributive::arg_min_max::{
    arg_max_null_functions, arg_min_null_functions,
};
use roastedduck_functions::aggregate::distributive::first_last_any::first_function;
use roastedduck_planner::expression::{AggregateType, BoundAggregateExpression, BoundWindowExpression};
use roastedduck_planner::operator::{JoinCondition, LogicalComparisonJoin};
use roastedduck_planner::{Expression, ExpressionKind, LogicalOperator};
use roastedduck_types::{LogicalType, LogicalTypeId};

use super::PlanGenerator;
use super::comparison_join::estimate_cardinality;
use crate::operator::PhysicalRef;
use crate::operators::{
    PhysicalAsOfJoin, PhysicalHashAggregate, PhysicalNestedLoopJoin, PhysicalProjection,
    PhysicalStreamingWindow,
};

/// The join type with the inputs swapped (`InverseJoinType`).
const fn inverse_join_type(join_type: JoinType) -> JoinType {
    match join_type {
        JoinType::Semi => JoinType::RightSemi,
        JoinType::Anti => JoinType::RightAnti,
        JoinType::Left => JoinType::Right,
        JoinType::Right => JoinType::Left,
        other => other,
    }
}

/// The comparison with its operands swapped (`FlipComparisonExpression`).
const fn flip_comparison(comparison: ExpressionType) -> ExpressionType {
    match comparison {
        ExpressionType::CompareLessthan => ExpressionType::CompareGreaterthan,
        ExpressionType::CompareGreaterthan => ExpressionType::CompareLessthan,
        ExpressionType::CompareLessthanorequalto => ExpressionType::CompareGreaterthanorequalto,
        ExpressionType::CompareGreaterthanorequalto => ExpressionType::CompareLessthanorequalto,
        other => other,
    }
}

/// Maps the column references of `expression` through `map`.
fn remap_references(expression: &mut Expression, map: &[u64]) {
    if let ExpressionKind::Reference(reference) = &mut expression.kind
        && let Some(&index) = map.get(reference.index as usize)
    {
        reference.index = index;
    }
    expression.for_each_child_mut(|child| remap_references(child, map));
}

/// The overload of `functions` for `arguments`, bound (`BindFunction` +
/// `BindAggregateFunction` for the exact or `ANY` signatures of `arg_min/max_null`).
fn bind_arg_min_max(
    functions: &AggregateFunctionSet,
    arguments: &[LogicalType],
) -> Result<(AggregateFunction, Option<Arc<dyn FunctionData>>)> {
    let matches = |function: &&AggregateFunction, any_first: bool, any_second: bool| {
        function.arguments.len() == 2
            && (if any_first {
                function.arguments[0].id() == LogicalTypeId::Any
            } else {
                function.arguments[0] == arguments[0]
            })
            && (if any_second {
                function.arguments[1].id() == LogicalTypeId::Any
            } else {
                function.arguments[1] == arguments[1]
            })
    };
    let mut function = functions
        .functions
        .iter()
        .find(|function| matches(function, false, false))
        .or_else(|| {
            functions
                .functions
                .iter()
                .find(|function| matches(function, true, false))
        })
        .or_else(|| {
            functions
                .functions
                .iter()
                .find(|function| matches(function, true, true))
        })
        .cloned()
        .ok_or_else(|| Error::internal(format!("no {} overload for ASOF loop join", functions.name)))?;
    let mut bind_info = None;
    if let Some(bind) = function.bind.clone() {
        let client = Arc::new(FunctionClient::default());
        let constants = vec![None; arguments.len()];
        let names = vec![roastedduck_common::ByteString::new(); arguments.len()];
        bind_info = bind(
            &mut function,
            &BindInput {
                argument_types: arguments,
                constants: &constants,
                aliases: &names,
                names: &names,
                client: &client,
                binder: None,
                arguments: None,
                aggregates: None,
                lambda: None,
            },
        )?;
    }
    Ok((function, bind_info))
}

/// A bound aggregate over column references.
fn aggregate(
    function: AggregateFunction,
    bind_info: Option<Arc<dyn FunctionData>>,
    children: Vec<Expression>,
) -> BoundAggregateExpression {
    BoundAggregateExpression {
        function,
        children,
        bind_info,
        aggr_type: AggregateType::NonDistinct,
        filter: None,
        order_bys: None,
    }
}

impl PlanGenerator {
    /// Plans a `LOGICAL_ASOF_JOIN` (`PlanAsOfJoin`).
    pub(crate) fn plan_asof_join(
        &mut self,
        mut join: LogicalComparisonJoin,
        children: Vec<LogicalOperator>,
        types: Vec<LogicalType>,
    ) -> Result<PhysicalRef> {
        let right_projection_map = std::mem::take(&mut join.right_projection_map);
        if !right_projection_map.is_empty()
            && !matches!(join.join_type, JoinType::Semi | JoinType::Anti | JoinType::Mark)
            && let [left, right] = children.as_slice()
        {
            // The as-of join outputs all right columns: project the kept ones on top.
            let left_width = left.types.len();
            let mut full_types = left.types.clone();
            full_types.extend(right.types.iter().cloned());
            let mut select_list: Vec<Expression> = (0..left_width)
                .map(|index| Expression::reference("", full_types[index].clone(), index as u64))
                .collect();
            for &column in &right_projection_map {
                let index = left_width + column as usize;
                select_list.push(Expression::reference("", full_types[index].clone(), index as u64));
            }
            // The predicate was resolved against the projected output.
            if let Some(predicate) = &mut join.predicate {
                let map: Vec<u64> = (0..left_width as u64)
                    .chain(
                        right_projection_map
                            .iter()
                            .map(|&column| left_width as u64 + column),
                    )
                    .collect();
                remap_references(predicate, &map);
            }
            let plan = self.plan_asof_join(join, children, full_types)?;
            return Ok(Arc::new(PhysicalProjection::new(types, select_list, plan)));
        }
        let mut children = children.into_iter();
        let (Some(left), Some(right)) = (children.next(), children.next()) else {
            return Err(Error::internal("ASOF join must have two inputs"));
        };
        let cardinalities = [
            estimate_cardinality(&left) as usize,
            estimate_cardinality(&right) as usize,
        ];
        let left_types = left.types.clone();
        let right_types = right.types.clone();
        let left = self.plan(left)?;
        let right = self.plan(right)?;
        let mut inequalities = 0;
        for condition in &join.conditions {
            match condition.comparison {
                ExpressionType::CompareEqual | ExpressionType::CompareNotDistinctFrom => {}
                ExpressionType::CompareGreaterthanorequalto
                | ExpressionType::CompareGreaterthan
                | ExpressionType::CompareLessthanorequalto
                | ExpressionType::CompareLessthan => inequalities += 1,
                _ => return Err(Error::internal("Invalid ASOF JOIN comparison")),
            }
        }
        if inequalities != 1 {
            return Err(Error::internal("Invalid ASOF JOIN comparison"));
        }
        let threshold = self.join_settings().asof_loop_join_threshold;
        if (join.predicate.is_some() || (cardinalities[0] as u64) < threshold)
            && let Some(plan) = plan_asof_loop_join(&join, &types, &left_types, &right_types, &left, &right)?
        {
            return Ok(plan);
        }
        if join.predicate.is_some() {
            return Err(Error::not_implemented("ASOF join with an arbitrary predicate"));
        }
        Ok(Arc::new(PhysicalAsOfJoin::new(
            types,
            join.join_type,
            join.conditions,
            left,
            right,
            cardinalities,
        )?))
    }
}

/// An ASOF join as a nested loop join of the right input with the numbered left input,
/// aggregated per left row to the closest match (`PlanAsOfLoopJoin`):
///
/// ```text
/// Π * \ pk
/// Γ pk; first(left columns), arg_min/max_null(right columns, inequality)
/// Π *, inequality
/// ⨝ (inputs swapped)
/// right    W pk := row_number()
///          left
/// ```
/// `None` where this plan does not apply.
fn plan_asof_loop_join(
    op: &LogicalComparisonJoin,
    types: &[LogicalType],
    probe_types: &[LogicalType],
    build_types: &[LogicalType],
    probe: &PhysicalRef,
    build: &PhysicalRef,
) -> Result<Option<PhysicalRef>> {
    let join_type = inverse_join_type(op.join_type);
    let build_width = build_types.len();
    let mut join_types = build_types.to_vec();
    join_types.extend(probe_types.iter().cloned());
    join_types.push(LogicalType::BIGINT);

    let mut conditions = Vec::with_capacity(op.conditions.len());
    let mut asof_index = None;
    let mut maximum = false;
    for (index, condition) in op.conditions.iter().enumerate() {
        conditions.push(JoinCondition {
            left: condition.right.clone(),
            right: condition.left.clone(),
            comparison: flip_comparison(condition.comparison),
        });
        match condition.comparison {
            ExpressionType::CompareGreaterthanorequalto | ExpressionType::CompareGreaterthan => {
                asof_index = Some(index);
                maximum = true;
            }
            ExpressionType::CompareLessthanorequalto | ExpressionType::CompareLessthan => {
                asof_index = Some(index);
                maximum = false;
            }
            ExpressionType::CompareEqual
            | ExpressionType::CompareNotequal
            | ExpressionType::CompareDistinctFrom => {}
            _ => return Ok(None),
        }
    }
    let Some(asof_index) = asof_index else {
        return Ok(None);
    };
    match join_type {
        JoinType::Semi | JoinType::Anti | JoinType::Mark | JoinType::Inner | JoinType::Right => {
            if !PhysicalNestedLoopJoin::is_supported(&op.conditions, op.join_type) {
                return Ok(None);
            }
        }
        _ => return Ok(None),
    }

    // The predicate sees the build columns first now.
    let predicate = op.predicate.clone().map(|mut predicate| {
        let map: Vec<u64> = (0..probe_types.len() as u64)
            .map(|column| column + build_width as u64)
            .chain(0..build_width as u64)
            .collect();
        remap_references(&mut predicate, &map);
        predicate
    });

    // Number the probe rows.
    let row_number = Expression::new(
        ExpressionType::WindowRowNumber,
        LogicalType::BIGINT,
        ExpressionKind::Window(Box::new(BoundWindowExpression {
            aggregate: None,
            bind_info: None,
            children: Vec::new(),
            partitions: Vec::new(),
            orders: Vec::new(),
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
    );
    let mut window_types = probe_types.to_vec();
    window_types.push(LogicalType::BIGINT);
    let window: PhysicalRef = Arc::new(PhysicalStreamingWindow::new(
        window_types,
        vec![row_number],
        Arc::clone(probe),
    )?);
    let join: PhysicalRef = Arc::new(PhysicalNestedLoopJoin::new(
        join_types.clone(),
        join_type,
        conditions,
        predicate,
        Arc::clone(build),
        window,
    ));

    // Project the inequality's build value as the ordering argument.
    let mut comp_list: Vec<Expression> = join_types
        .iter()
        .enumerate()
        .map(|(index, ty)| Expression::reference("", ty.clone(), index as u64))
        .collect();
    let comp_expression = op.conditions[asof_index].right.clone();
    let comp_type = comp_expression.return_type.clone();
    comp_list.push(comp_expression);
    let mut comp_types = join_types.clone();
    comp_types.push(comp_type.clone());
    let comp_index = comp_types.len() as u64 - 1;
    let comp_projection: PhysicalRef = Arc::new(PhysicalProjection::new(comp_types, comp_list, join));

    let mut aggregate_types = vec![LogicalType::BIGINT];
    let mut aggregates = Vec::new();
    for (index, ty) in probe_types.iter().enumerate() {
        aggregate_types.push(ty.clone());
        let column = Expression::reference("", ty.clone(), (build_width + index) as u64);
        aggregates.push(aggregate(first_function::<false, false>(ty), None, vec![column]));
    }
    let functions = if maximum {
        arg_max_null_functions()
    } else {
        arg_min_null_functions()
    };
    for (index, ty) in build_types.iter().enumerate() {
        let (function, bind_info) = bind_arg_min_max(&functions, &[ty.clone(), comp_type.clone()])?;
        if &function.return_type != ty {
            return Ok(None);
        }
        aggregate_types.push(ty.clone());
        let children = vec![
            Expression::reference("", ty.clone(), index as u64),
            Expression::reference("", comp_type.clone(), comp_index),
        ];
        aggregates.push(aggregate(function, bind_info, children));
    }
    let group = Expression::reference("", LogicalType::BIGINT, join_types.len() as u64 - 1);
    let aggregate: PhysicalRef = Arc::new(PhysicalHashAggregate::new(
        aggregate_types.clone(),
        vec![group],
        aggregates,
        Vec::new(),
        Vec::new(),
        comp_projection,
    )?);
    let project_list = aggregate_types
        .iter()
        .enumerate()
        .skip(1)
        .map(|(index, ty)| Expression::reference("", ty.clone(), index as u64))
        .collect();
    Ok(Some(Arc::new(PhysicalProjection::new(
        types.to_vec(),
        project_list,
        aggregate,
    ))))
}
